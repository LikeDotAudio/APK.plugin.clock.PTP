// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! Packet inter-arrival: rate, jitter and loss, one message stream at a time.

use crate::flow::PortKey;
use crate::message::{MessageType, PtpMessage};
use std::collections::HashMap;
use std::time::Instant;

/// One stream of messages: a single port emitting a single message type.
///
/// PTP keeps a separate `sequenceId` counter per message type per port, so
/// this is also the exact scope within which a sequence gap means a message
/// went missing. Keyed any wider — per port, say — a Sync and an Announce
/// share a counter space they do not share on the wire, and every second
/// packet reads as a hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamKey {
    pub port: PortKey,
    pub message_type: MessageType,
}

/// Arrival timing for one stream, accumulated packet by packet.
///
/// WHAT THESE NUMBERS ARE, AND WHAT THEY ARE NOT
///
/// Every interval here is measured between two arrivals at *this capture
/// point*, with a userspace read in the middle. That makes the spread a FLOOR
/// on the network's packet delay variation, never the network's figure alone:
/// our own scheduling is in it. It is still the right instrument for the three
/// questions a dropout actually raises —
///
///   * is the master still emitting at the rate it advertises (`rate_error`),
///   * is the spacing coming apart (`jitter`, `variation_peak`),
///   * are messages going missing (`lost`, `loss_ratio`)
///
/// — because all three are answered by *change*, and the constant part of our
/// own overhead cancels out of a change. Reporting an absolute nanosecond
/// figure as "the network's jitter" is the one reading this must not invite,
/// which is why nothing here is named for the network.
///
/// Variance is Welford's, not a sum of squares: a Sync stream at 8/s over an
/// hour is 28,800 samples of a number near 0.125, and the naive form loses the
/// spread into the mean long before that.
#[derive(Debug, Clone)]
pub struct Cadence {
    pub count: u64,
    /// The rate this stream ADVERTISES, from `logMessageInterval`, in seconds.
    ///
    /// Only set for the message types whose interval field describes their own
    /// rate. On a Delay_Resp the same byte carries `logMinDelayReqInterval` —
    /// an instruction to the requester, not a description of this stream — and
    /// reading it as a cadence would report every well-behaved master as
    /// running at the wrong rate.
    pub expected_interval: Option<f64>,
    pub first_at: Instant,
    pub last_at: Instant,
    pub intervals: u64,
    running_mean: f64,
    squared_deviations: f64,
    pub shortest: f64,
    pub longest: f64,
    previous_interval: Option<f64>,
    pub variation_samples: u64,
    variation_total: f64,
    /// Largest single arrival-to-arrival swing seen, in seconds. This is the
    /// IPDV of RFC 3393 taken pairwise, and it is what a buffer has to absorb.
    pub variation_peak: f64,
    previous_sequence: Option<u16>,
    pub lost: u64,
    pub reordered: u64,
    pub duplicated: u64,
}

impl Cadence {
    fn new(at: Instant) -> Self {
        Self {
            count: 0,
            expected_interval: None,
            first_at: at,
            last_at: at,
            intervals: 0,
            running_mean: 0.0,
            squared_deviations: 0.0,
            shortest: f64::INFINITY,
            longest: 0.0,
            previous_interval: None,
            variation_samples: 0,
            variation_total: 0.0,
            variation_peak: 0.0,
            previous_sequence: None,
            lost: 0,
            reordered: 0,
            duplicated: 0,
        }
    }

    /// Mean seconds between arrivals, once there are two arrivals to divide.
    pub fn mean_interval(&self) -> Option<f64> {
        (self.intervals > 0).then_some(self.running_mean)
    }

    /// Standard deviation of the interval, in seconds — the spread that a
    /// receiver's buffer is sized against.
    ///
    /// Needs two intervals, not two packets: one interval has no spread, and
    /// the sample variance would divide by zero to say so.
    pub fn jitter(&self) -> Option<f64> {
        (self.intervals > 1)
            .then(|| (self.squared_deviations / (self.intervals - 1) as f64).sqrt())
    }

    /// Mean magnitude of the swing between consecutive intervals.
    pub fn variation_mean(&self) -> Option<f64> {
        (self.variation_samples > 0)
            .then(|| self.variation_total / self.variation_samples as f64)
    }

    /// How far the observed rate sits from the advertised one, as a fraction.
    ///
    /// `0.02` is a master running 2% slow. A master that has stopped
    /// disciplining itself drifts here before it drifts anywhere a listener
    /// can hear, which is the whole reason to watch it.
    pub fn rate_error(&self) -> Option<f64> {
        let expected = self.expected_interval?;
        let observed = self.mean_interval()?;
        (expected > 0.0).then(|| (observed - expected) / expected)
    }

    /// Fraction of this stream's messages that never arrived.
    ///
    /// Counted from sequence gaps, so a message the capture itself dropped is
    /// indistinguishable from one the network dropped. Both are real to a
    /// listener downstream of this NIC, and neither is invented.
    pub fn loss_ratio(&self) -> Option<f64> {
        let expected = self.count + self.lost;
        (expected > 0).then(|| self.lost as f64 / expected as f64)
    }

    fn observe(&mut self, message: &PtpMessage, at: Instant) {
        if self.count > 0 {
            let interval = at.saturating_duration_since(self.last_at).as_secs_f64();
            self.intervals += 1;
            let deviation = interval - self.running_mean;
            self.running_mean += deviation / self.intervals as f64;
            self.squared_deviations += deviation * (interval - self.running_mean);
            self.shortest = self.shortest.min(interval);
            self.longest = self.longest.max(interval);
            if let Some(previous) = self.previous_interval {
                let swing = (interval - previous).abs();
                self.variation_samples += 1;
                self.variation_total += swing;
                self.variation_peak = self.variation_peak.max(swing);
            }
            self.previous_interval = Some(interval);
        }

        self.count += 1;
        self.last_at = at;
        if advertises_own_rate(message.message_type) {
            self.expected_interval = Some(message.interval_secs());
        }
        self.follow_sequence(message.sequence_id);
    }

    /// Account for one sequence number against the last one seen.
    ///
    /// Sequence IDs are 16-bit and wrap, so the comparison is a wrapping
    /// difference and not `>`. Half the space is read as forward and half as
    /// backward, which is the only reading that survives the wrap: at
    /// 65535 → 0 the naive form reports 65535 lost messages on a stream that
    /// lost none, and does it once per two hours on a gPTP Sync.
    fn follow_sequence(&mut self, sequence: u16) {
        let Some(previous) = self.previous_sequence else {
            self.previous_sequence = Some(sequence);
            return;
        };
        match sequence.wrapping_sub(previous) {
            0 => self.duplicated += 1,
            1 => self.previous_sequence = Some(sequence),
            forward if forward < 0x8000 => {
                self.lost += u64::from(forward) - 1;
                self.previous_sequence = Some(sequence);
            }
            // Behind the highest seen: a reorder, or a talker that restarted
            // its counter. Either way the gap it appears to leave is not loss,
            // and the high-water mark must not move backwards.
            _ => self.reordered += 1,
        }
    }
}

/// Whether this message type's `logMessageInterval` describes its own rate.
fn advertises_own_rate(message_type: MessageType) -> bool {
    matches!(
        message_type,
        MessageType::Sync | MessageType::Announce | MessageType::PdelayReq
    )
}

/// Every stream seen, keyed by port and message type.
#[derive(Default)]
pub struct ArrivalWatch {
    pub streams: HashMap<StreamKey, Cadence>,
}

impl ArrivalWatch {
    pub fn observe(&mut self, message: &PtpMessage, at: Instant) {
        let key = StreamKey { port: PortKey::of(message), message_type: message.message_type };
        self.streams.entry(key).or_insert_with(|| Cadence::new(at)).observe(message, at);
    }

    pub fn stream(&self, port: &PortKey, message_type: MessageType) -> Option<&Cadence> {
        self.streams.get(&StreamKey { port: *port, message_type })
    }

    /// Forget streams whose port is gone, so the memory follows the clock table.
    pub fn retain_ports(&mut self, keep: impl Fn(&PortKey) -> bool) {
        self.streams.retain(|key, _| keep(&key.port));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{parse, Variant};
    use std::time::Duration;

    const GM: [u8; 8] = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];

    /// A Sync with `logMessageInterval` -3, the standard gPTP 125ms rate.
    fn sync(sequence: u16) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = 1 << 4;
        payload[1] = 0x02;
        payload[6] = 0x02;
        payload[20..28].copy_from_slice(&GM);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        payload[30..32].copy_from_slice(&sequence.to_be_bytes());
        payload[33] = 0xFD;
        payload.extend_from_slice(&[0u8; 10]);
        parse(&payload, Variant::V2Ethernet).unwrap()
    }

    fn feed(watch: &mut ArrivalWatch, start: Instant, gaps: &[f64], from: u16) {
        let mut at = start;
        let mut sequence = from;
        watch.observe(&sync(sequence), at);
        for gap in gaps {
            at += Duration::from_secs_f64(*gap);
            sequence = sequence.wrapping_add(1);
            watch.observe(&sync(sequence), at);
        }
    }

    #[test]
    fn a_perfectly_spaced_stream_has_no_jitter() {
        let mut watch = ArrivalWatch::default();
        feed(&mut watch, Instant::now(), &[0.125; 8], 0);
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.count, 9);
        assert!((cadence.mean_interval().unwrap() - 0.125).abs() < 1e-9);
        assert!(cadence.jitter().unwrap() < 1e-9, "even spacing is zero spread");
        assert_eq!(cadence.variation_peak, 0.0);
        assert_eq!(cadence.lost, 0);
    }

    /// The advertised rate is the thing an observed rate is judged against —
    /// a master 20% slow is a fault long before anything sounds wrong.
    #[test]
    fn rate_error_is_measured_against_the_advertised_interval() {
        let mut watch = ArrivalWatch::default();
        feed(&mut watch, Instant::now(), &[0.150; 8], 0);
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.expected_interval, Some(0.125));
        let error = cadence.rate_error().unwrap();
        assert!((error - 0.2).abs() < 1e-6, "0.150 vs 0.125 is +20%, got {error}");
    }

    /// One late packet in an otherwise even stream is what a buffer has to
    /// absorb, and the peak swing is the number that sizes it.
    #[test]
    fn a_single_late_packet_shows_up_as_peak_variation() {
        let mut watch = ArrivalWatch::default();
        feed(&mut watch, Instant::now(), &[0.125, 0.125, 0.400, 0.125, 0.125], 0);
        let cadence = watch.streams.values().next().unwrap();
        // 0.400 - 0.125 arriving, then 0.125 - 0.400 leaving: both 0.275.
        assert!((cadence.variation_peak - 0.275).abs() < 1e-9);
        assert!((cadence.longest - 0.400).abs() < 1e-9);
        assert!(cadence.jitter().unwrap() > 0.1);
    }

    #[test]
    fn a_sequence_gap_is_counted_as_loss() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        watch.observe(&sync(10), at);
        watch.observe(&sync(14), at + Duration::from_millis(500));
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.lost, 3, "11, 12 and 13 never arrived");
        assert!((cadence.loss_ratio().unwrap() - 0.6).abs() < 1e-9);
    }

    /// The bug this wrapping comparison exists to prevent: at 65535 -> 0 a
    /// naive `sequence - previous` reports 65535 lost on a stream that lost
    /// nothing, once every two hours at the standard gPTP Sync rate.
    #[test]
    fn the_sequence_wrap_is_not_sixty_five_thousand_lost_messages() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        watch.observe(&sync(65534), at);
        watch.observe(&sync(65535), at + Duration::from_millis(125));
        watch.observe(&sync(0), at + Duration::from_millis(250));
        watch.observe(&sync(1), at + Duration::from_millis(375));
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.lost, 0);
        assert_eq!(cadence.count, 4);
    }

    #[test]
    fn a_repeated_sequence_is_a_duplicate_not_a_loss() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        watch.observe(&sync(7), at);
        watch.observe(&sync(7), at + Duration::from_millis(1));
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.duplicated, 1);
        assert_eq!(cadence.lost, 0);
    }

    /// A reordered arrival must not drag the high-water mark backwards, or
    /// the next in-order message reads as thousands of lost ones.
    #[test]
    fn a_reordered_arrival_does_not_manufacture_loss() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        watch.observe(&sync(20), at);
        watch.observe(&sync(21), at + Duration::from_millis(125));
        watch.observe(&sync(19), at + Duration::from_millis(130));
        watch.observe(&sync(22), at + Duration::from_millis(250));
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.reordered, 1);
        assert_eq!(cadence.lost, 0);
    }

    /// Sync and Announce keep separate sequence counters on the wire. Merged
    /// into one stream they would read as a hole every other packet.
    #[test]
    fn message_types_are_separate_streams() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        let mut announce = sync(1000);
        announce.message_type = MessageType::Announce;
        watch.observe(&sync(5), at);
        watch.observe(&announce, at);
        assert_eq!(watch.streams.len(), 2);
        for cadence in watch.streams.values() {
            assert_eq!(cadence.lost, 0);
        }
    }

    /// One packet is not a rate, and one interval is not a spread. Both must
    /// say so rather than report a confident zero.
    #[test]
    fn too_few_samples_report_nothing_rather_than_zero() {
        let mut watch = ArrivalWatch::default();
        let at = Instant::now();
        watch.observe(&sync(1), at);
        let cadence = watch.streams.values().next().unwrap();
        assert_eq!(cadence.mean_interval(), None);
        assert_eq!(cadence.jitter(), None);
        assert_eq!(cadence.rate_error(), None);

        watch.observe(&sync(2), at + Duration::from_millis(125));
        let cadence = watch.streams.values().next().unwrap();
        assert!(cadence.mean_interval().is_some());
        assert_eq!(cadence.jitter(), None, "one interval has no spread");
    }
}
