// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! Offset from master, clock drift, and the delays a passive listener can
//! actually measure.

use crate::flow::PortKey;
use crate::message::{FLAG_PTP_TIMESCALE, MessageType, PtpMessage, Timestamp, Variant};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Nanoseconds in a second, as the integer the timestamp arithmetic uses.
const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// How long an unmatched half-exchange is held before it is forgotten.
///
/// The same reasoning as `FlowTracker`'s: sequence IDs wrap, and a Sync whose
/// Follow_Up was lost would otherwise be completed by a wrapped sequence
/// 65536 messages later and produce one enormous fictional offset.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on unmatched halves held at once, so a flood cannot grow the map without
/// bound between sweeps.
const PAIRING_CAPACITY: usize = 4096;

/// A Sync waiting for the Follow_Up that carries its real departure time.
struct PendingSync {
    arrived_at: SystemTime,
    seen_at: Instant,
    correction_nanos: f64,
    ptp_timescale: bool,
}

/// A Pdelay_Resp waiting for the Follow_Up that carries `t3`.
struct PendingPeer {
    request_received: Timestamp,
    seen_at: Instant,
}

/// Offset samples for one master port, as observed here.
///
/// WHAT THE ABSOLUTE NUMBER CONTAINS
///
/// A passive listener never sees `t2` — the slave's receipt stamp — so it
/// cannot compute the `offsetFromMaster` a slave computes. What it can compute
/// is the gap between the master's own departure stamp and this host's clock
/// at the moment the packet landed:
///
/// ```text
///   apparent offset = (arrival on this host, in TAI)
///                   - (t1 + every correctionField on the way)
/// ```
///
/// which is the sum of three things: this host's own clock error against the
/// grandmaster, the one-way network delay to this capture point, and the
/// latency of our own capture stack. So the absolute figure is a bound, not a
/// measurement, and it is named `apparent` everywhere for that reason.
///
/// WHY IT IS WORTH KEEPING ANYWAY
///
/// Two of those three terms are constant over a capture. The variation is
/// therefore the grandmaster's stability as this point on the network
/// experiences it, and the slope is the frequency error between this host and
/// that grandmaster in nanoseconds per second — the number that goes wrong
/// first when a master falls out of lock, hours before anything is audible.
///
/// Samples are accumulated against `base_nanos` rather than as absolute epoch
/// nanoseconds. An absolute PTP timestamp is around 1.8e18 ns; `f64` carries
/// 53 bits, so accumulating those directly throws away the nanoseconds the
/// whole measurement is about.
#[derive(Debug, Clone)]
pub struct MasterOffset {
    pub samples: u64,
    base_nanos: i128,
    running_mean: f64,
    squared_deviations: f64,
    relative_lowest: f64,
    relative_highest: f64,
    latest_relative: f64,
    /// Least-squares accumulators, seconds against relative nanoseconds.
    elapsed_total: f64,
    elapsed_squared_total: f64,
    relative_total: f64,
    product_total: f64,
    first_at: Instant,
    pub last_at: Instant,
    /// True once an Announce in this domain has supplied `currentUtcOffset`.
    /// Without it the arrival stamp is UTC and `t1` is TAI, and the two differ
    /// by the leap-second count — 37 seconds of pure fiction in the answer.
    pub utc_offset_applied: bool,
    /// True when the master flags its timestamps as being on the PTP
    /// timescale. When it is false the epoch is arbitrary, the absolute figure
    /// means nothing at all, and only the variation and the drift survive.
    pub ptp_timescale: bool,
}

impl MasterOffset {
    fn new(nanos: i128, at: Instant, utc_offset_applied: bool, ptp_timescale: bool) -> Self {
        Self {
            samples: 0,
            base_nanos: nanos,
            running_mean: 0.0,
            squared_deviations: 0.0,
            relative_lowest: f64::INFINITY,
            relative_highest: f64::NEG_INFINITY,
            latest_relative: 0.0,
            elapsed_total: 0.0,
            elapsed_squared_total: 0.0,
            relative_total: 0.0,
            product_total: 0.0,
            first_at: at,
            last_at: at,
            utc_offset_applied,
            ptp_timescale,
        }
    }

    fn push(&mut self, nanos: i128, at: Instant) {
        let relative = (nanos - self.base_nanos) as f64;
        self.samples += 1;
        let deviation = relative - self.running_mean;
        self.running_mean += deviation / self.samples as f64;
        self.squared_deviations += deviation * (relative - self.running_mean);
        self.relative_lowest = self.relative_lowest.min(relative);
        self.relative_highest = self.relative_highest.max(relative);
        self.latest_relative = relative;

        let elapsed = at.saturating_duration_since(self.first_at).as_secs_f64();
        self.elapsed_total += elapsed;
        self.elapsed_squared_total += elapsed * elapsed;
        self.relative_total += relative;
        self.product_total += elapsed * relative;
        self.last_at = at;
    }

    /// Most recent apparent offset, in nanoseconds.
    pub fn latest_nanos(&self) -> i128 {
        self.base_nanos + self.latest_relative as i128
    }

    /// Mean apparent offset over the capture, in nanoseconds.
    pub fn mean_nanos(&self) -> i128 {
        self.base_nanos + self.running_mean as i128
    }

    /// Spread of the apparent offset — the part of it that is not constant,
    /// and therefore the part that is about the clock rather than about the
    /// path. Standard deviation, in nanoseconds.
    pub fn spread_nanos(&self) -> Option<f64> {
        (self.samples > 1).then(|| (self.squared_deviations / (self.samples - 1) as f64).sqrt())
    }

    /// Worst-to-best excursion of the apparent offset, in nanoseconds.
    pub fn peak_to_peak_nanos(&self) -> Option<f64> {
        (self.samples > 1).then_some(self.relative_highest - self.relative_lowest)
    }

    /// Frequency error against this master, in nanoseconds per second.
    ///
    /// The least-squares slope of apparent offset against time. Two samples a
    /// millisecond apart give a slope that is all noise, so this refuses to
    /// answer until the samples span a window long enough for the fit to mean
    /// something.
    pub fn drift_per_second(&self) -> Option<f64> {
        if self.samples < 8 {
            return None;
        }
        let count = self.samples as f64;
        let span = self
            .last_at
            .saturating_duration_since(self.first_at)
            .as_secs_f64();
        if span < 1.0 {
            return None;
        }
        let denominator = count * self.elapsed_squared_total - self.elapsed_total.powi(2);
        (denominator.abs() > f64::EPSILON).then(|| {
            (count * self.product_total - self.elapsed_total * self.relative_total) / denominator
        })
    }

    /// Seconds of wall time the samples span.
    pub fn observed_seconds(&self) -> f64 {
        self.last_at
            .saturating_duration_since(self.first_at)
            .as_secs_f64()
    }
}

/// The delays a passive listener CAN measure, per port.
///
/// Not `meanPathDelay`: that needs `t2` and `t3`, which only the two endpoints
/// hold. What is on the wire and is genuinely a delay is:
///
/// * `correction` — the accumulated `correctionField`, which every transparent
///   clock between the grandmaster and here adds its own residence time to.
///   This IS the switch fabric's contribution to the path, stated by the
///   fabric itself.
/// * `turnaround` — `t3 - t2` from a peer-delay responder, both stamps carried
///   in its own two messages and both on its own clock, so the subtraction is
///   valid without knowing anything about anyone else's time.
#[derive(Debug, Clone, Default)]
pub struct PathDelay {
    pub correction_samples: u64,
    pub correction_total: f64,
    pub correction_peak: f64,
    pub turnaround_samples: u64,
    pub turnaround_total: f64,
    pub turnaround_peak: f64,
}

impl PathDelay {
    /// Mean accumulated transparent-clock residence, in nanoseconds.
    pub fn mean_correction(&self) -> Option<f64> {
        (self.correction_samples > 0)
            .then(|| self.correction_total / self.correction_samples as f64)
    }

    /// Mean peer-delay responder turnaround, in nanoseconds.
    pub fn mean_turnaround(&self) -> Option<f64> {
        (self.turnaround_samples > 0)
            .then(|| self.turnaround_total / self.turnaround_samples as f64)
    }
}

/// Offset and delay measurement across every master on the wire.
#[derive(Default)]
pub struct OffsetWatch {
    pub masters: HashMap<PortKey, MasterOffset>,
    pub delays: HashMap<PortKey, PathDelay>,
    utc_offsets: HashMap<(u8, Variant), i16>,
    pending_sync: HashMap<(PortKey, u16), PendingSync>,
    pending_peer: HashMap<(PortKey, u16), PendingPeer>,
}

impl OffsetWatch {
    /// Feed one message, with the wall clock it landed on this host at.
    pub fn observe(&mut self, message: &PtpMessage, arrived_at: SystemTime, at: Instant) {
        let key = PortKey::of(message);
        match message.message_type {
            MessageType::Announce => {
                if let Some(announce) = &message.announce {
                    self.utc_offsets.insert(
                        (message.domain, message.variant),
                        announce.current_utc_offset,
                    );
                }
            }
            MessageType::Sync => {
                self.note_correction(key, message.correction_ns);
                if message.is_two_step() {
                    self.remember_sync(key, message, arrived_at, at);
                } else if let Some(timestamp) = live_timestamp(message) {
                    self.sample(
                        key,
                        message,
                        timestamp,
                        arrived_at,
                        at,
                        message.correction_ns,
                    );
                }
            }
            MessageType::FollowUp => {
                self.note_correction(key, message.correction_ns);
                let Some(pending) = self.pending_sync.remove(&(key, message.sequence_id)) else {
                    return;
                };
                let Some(timestamp) = live_timestamp(message) else {
                    return;
                };
                // The Sync is the packet that crossed the network; the
                // Follow_Up only carries the number. Pairing the Follow_Up's
                // t1 with the Follow_Up's OWN arrival would fold the gap
                // between the two packets into the offset, which is a
                // property of the master's transmit path and not of the
                // clock.
                let correction = pending.correction_nanos + message.correction_ns;
                let ptp_timescale = pending.ptp_timescale;
                self.sample_with(
                    key,
                    message.domain,
                    message.variant,
                    timestamp,
                    pending.arrived_at,
                    at,
                    correction,
                    ptp_timescale,
                );
            }
            MessageType::PdelayResp => {
                if message.is_two_step()
                    && let Some(timestamp) = live_timestamp(message)
                {
                    self.sweep_peer(at);
                    self.pending_peer.insert(
                        (key, message.sequence_id),
                        PendingPeer {
                            request_received: timestamp,
                            seen_at: at,
                        },
                    );
                }
            }
            MessageType::PdelayRespFollowUp => {
                let Some(pending) = self.pending_peer.remove(&(key, message.sequence_id)) else {
                    return;
                };
                let Some(response_origin) = live_timestamp(message) else {
                    return;
                };
                let turnaround = as_nanos(&response_origin) - as_nanos(&pending.request_received);
                // Both stamps are the responder's own, so a negative result is
                // not a clock disagreement — it is a malformed pair, and
                // averaging it in would quietly lower every number here.
                if turnaround >= 0 {
                    let delay = self.delays.entry(key).or_default();
                    delay.turnaround_samples += 1;
                    delay.turnaround_total += turnaround as f64;
                    delay.turnaround_peak = delay.turnaround_peak.max(turnaround as f64);
                }
            }
            _ => {}
        }
    }

    /// The offset record for one master port, if any Sync has been paired.
    pub fn master(&self, key: &PortKey) -> Option<&MasterOffset> {
        self.masters.get(key)
    }

    pub fn delay(&self, key: &PortKey) -> Option<&PathDelay> {
        self.delays.get(key)
    }

    /// Forget ports that are gone, so the memory follows the clock table.
    pub fn retain_ports(&mut self, keep: impl Fn(&PortKey) -> bool) {
        self.masters.retain(|key, _| keep(key));
        self.delays.retain(|key, _| keep(key));
        self.pending_sync.retain(|(key, _), _| keep(key));
        self.pending_peer.retain(|(key, _), _| keep(key));
    }

    fn note_correction(&mut self, key: PortKey, correction: f64) {
        let delay = self.delays.entry(key).or_default();
        delay.correction_samples += 1;
        delay.correction_total += correction;
        delay.correction_peak = delay.correction_peak.max(correction.abs());
    }

    fn remember_sync(
        &mut self,
        key: PortKey,
        message: &PtpMessage,
        arrived_at: SystemTime,
        at: Instant,
    ) {
        self.sweep_sync(at);
        self.pending_sync.insert(
            (key, message.sequence_id),
            PendingSync {
                arrived_at,
                seen_at: at,
                correction_nanos: message.correction_ns,
                ptp_timescale: message.flags & FLAG_PTP_TIMESCALE != 0,
            },
        );
    }

    fn sample(
        &mut self,
        key: PortKey,
        message: &PtpMessage,
        origin: Timestamp,
        arrived_at: SystemTime,
        at: Instant,
        correction: f64,
    ) {
        let ptp_timescale = message.flags & FLAG_PTP_TIMESCALE != 0;
        self.sample_with(
            key,
            message.domain,
            message.variant,
            origin,
            arrived_at,
            at,
            correction,
            ptp_timescale,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_with(
        &mut self,
        key: PortKey,
        domain: u8,
        variant: Variant,
        origin: Timestamp,
        arrived_at: SystemTime,
        at: Instant,
        correction: f64,
        ptp_timescale: bool,
    ) {
        let Ok(since_epoch) = arrived_at.duration_since(UNIX_EPOCH) else {
            return;
        };
        let utc_offset = self.utc_offsets.get(&(domain, variant)).copied();
        let arrival_nanos =
            since_epoch.as_nanos() as i128 + i128::from(utc_offset.unwrap_or(0)) * NANOS_PER_SECOND;
        let offset = arrival_nanos - as_nanos(&origin) - correction.round() as i128;

        let record = self
            .masters
            .entry(key)
            .or_insert_with(|| MasterOffset::new(offset, at, utc_offset.is_some(), ptp_timescale));
        record.utc_offset_applied = utc_offset.is_some();
        record.ptp_timescale = ptp_timescale;
        record.push(offset, at);
    }

    fn sweep_sync(&mut self, now: Instant) {
        if self.pending_sync.len() < PAIRING_CAPACITY {
            return;
        }
        self.pending_sync
            .retain(|_, pending| now.saturating_duration_since(pending.seen_at) < PAIRING_TIMEOUT);
    }

    fn sweep_peer(&mut self, now: Instant) {
        if self.pending_peer.len() < PAIRING_CAPACITY {
            return;
        }
        self.pending_peer
            .retain(|_, pending| now.saturating_duration_since(pending.seen_at) < PAIRING_TIMEOUT);
    }
}

/// A timestamp that is actually populated. A one-step transmitter that cannot
/// stamp on the wire sends zeroes, and treating that as an epoch time makes the
/// offset the age of the universe.
fn live_timestamp(message: &PtpMessage) -> Option<Timestamp> {
    message.timestamp.filter(|timestamp| !timestamp.is_zero())
}

fn as_nanos(timestamp: &Timestamp) -> i128 {
    timestamp.seconds as i128 * NANOS_PER_SECOND + i128::from(timestamp.nanos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::parse;

    const GM: [u8; 8] = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];

    fn message(
        message_type: u8,
        sequence: u16,
        two_step: bool,
        timestamp: Option<(u64, u32)>,
        correction_nanos: i64,
    ) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = message_type;
        payload[1] = 0x02;
        // twoStep in octet 0 of flagField, ptpTimescale in octet 1.
        payload[6] = if two_step { 0x02 } else { 0x00 };
        payload[7] = 0x08;
        payload[8..16].copy_from_slice(&(correction_nanos << 16).to_be_bytes());
        payload[20..28].copy_from_slice(&GM);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        payload[30..32].copy_from_slice(&sequence.to_be_bytes());
        payload[33] = 0xFD;
        let (seconds, nanos) = timestamp.unwrap_or((0, 0));
        let mut body = vec![0u8; 10];
        body[0..6].copy_from_slice(&seconds.to_be_bytes()[2..8]);
        body[6..10].copy_from_slice(&nanos.to_be_bytes());
        if matches!(message_type & 0x0F, 0x3 | 0x9 | 0xA) {
            body.extend_from_slice(&[0u8; 10]);
        }
        payload.extend_from_slice(&body);
        parse(&payload, Variant::V2Udp).unwrap()
    }

    fn announce(utc_offset: i16) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = 0xB;
        payload[1] = 0x02;
        payload[20..28].copy_from_slice(&GM);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        let mut body = vec![0u8; 30];
        body[10..12].copy_from_slice(&utc_offset.to_be_bytes());
        body[14] = 6;
        body[19..27].copy_from_slice(&GM);
        payload.extend_from_slice(&body);
        parse(&payload, Variant::V2Udp).unwrap()
    }

    fn wall(seconds: u64, nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(seconds, nanos)
    }

    /// A one-step Sync is measurable the moment it lands: the departure stamp
    /// is in the packet.
    #[test]
    fn a_one_step_sync_yields_an_offset_immediately() {
        let mut watch = OffsetWatch::default();
        let at = Instant::now();
        watch.observe(&announce(37), wall(1_000_000_000, 0), at);
        // Departure at TAI 1_000_000_037.0, arrival at UTC 1_000_000_000 plus
        // 250us. With currentUtcOffset 37 applied the two meet, and the
        // remainder is the 250us this host is behind plus the path.
        watch.observe(
            &message(0x0, 1, false, Some((1_000_000_037, 0)), 0),
            wall(1_000_000_000, 250_000),
            at,
        );
        let record = watch.masters.values().next().unwrap();
        assert_eq!(record.samples, 1);
        assert!(record.utc_offset_applied);
        assert_eq!(record.latest_nanos(), 250_000);
    }

    /// Without an Announce there is no `currentUtcOffset`, and the 37-second
    /// TAI-UTC gap lands in the answer. The record has to say which it is.
    #[test]
    fn an_unapplied_utc_offset_is_declared_not_hidden() {
        let mut watch = OffsetWatch::default();
        watch.observe(
            &message(0x0, 1, false, Some((1_000_000_037, 0)), 0),
            wall(1_000_000_000, 250_000),
            Instant::now(),
        );
        let record = watch.masters.values().next().unwrap();
        assert!(!record.utc_offset_applied);
        assert_eq!(record.latest_nanos(), 250_000 - 37 * 1_000_000_000);
    }

    /// A two-step Sync is measured against the SYNC's arrival and the
    /// FOLLOW_UP's timestamp. Using the Follow_Up's own arrival would fold the
    /// master's transmit gap into the offset.
    #[test]
    fn a_two_step_pair_uses_the_syncs_arrival_and_the_follow_ups_stamp() {
        let mut watch = OffsetWatch::default();
        let at = Instant::now();
        watch.observe(&announce(37), wall(1_000_000_000, 0), at);
        watch.observe(
            &message(0x0, 9, true, None, 0),
            wall(1_000_000_000, 250_000),
            at,
        );
        assert!(watch.masters.is_empty(), "a Sync alone is not yet a sample");
        watch.observe(
            &message(0x8, 9, true, Some((1_000_000_037, 0)), 0),
            // Forty milliseconds later, and it must not appear in the answer.
            wall(1_000_000_000, 40_250_000),
            at,
        );
        let record = watch.masters.values().next().unwrap();
        assert_eq!(record.latest_nanos(), 250_000);
    }

    /// The correctionField is the switch fabric's own statement of how long it
    /// held the packet. Leaving it out reports the fabric's latency as clock
    /// error.
    #[test]
    fn the_correction_field_is_subtracted_from_the_offset() {
        let mut watch = OffsetWatch::default();
        let at = Instant::now();
        watch.observe(&announce(37), wall(1_000_000_000, 0), at);
        watch.observe(
            &message(0x0, 1, false, Some((1_000_000_037, 0)), 50_000),
            wall(1_000_000_000, 250_000),
            at,
        );
        let record = watch.masters.values().next().unwrap();
        assert_eq!(
            record.latest_nanos(),
            200_000,
            "250us arrival less 50us residence"
        );
        let delay = watch.delays.values().next().unwrap();
        assert_eq!(delay.mean_correction(), Some(50_000.0));
    }

    /// A master running fast against this host produces a straight line, and
    /// the slope of it is the frequency error in ns/s.
    #[test]
    fn a_linear_walk_is_reported_as_drift() {
        let mut watch = OffsetWatch::default();
        let start = Instant::now();
        watch.observe(&announce(37), wall(1_000_000_000, 0), start);
        // Ten Syncs a second apart, each 1us further behind than the last.
        for step in 0..20u64 {
            watch.observe(
                &message(0x0, step as u16, false, Some((1_000_000_037 + step, 0)), 0),
                wall(1_000_000_000 + step, 1_000 * step as u32),
                start + Duration::from_secs(step),
            );
        }
        let record = watch.masters.values().next().unwrap();
        let drift = record.drift_per_second().unwrap();
        assert!((drift - 1_000.0).abs() < 1.0, "1us per second, got {drift}");
        assert!(record.spread_nanos().unwrap() > 0.0);
    }

    /// Too few samples over too short a window is noise, not a drift rate.
    #[test]
    fn drift_refuses_to_answer_from_a_handful_of_samples() {
        let mut watch = OffsetWatch::default();
        let start = Instant::now();
        for step in 0..4u64 {
            watch.observe(
                &message(0x0, step as u16, false, Some((1_000_000_037 + step, 0)), 0),
                wall(1_000_000_000 + step, 0),
                start + Duration::from_secs(step),
            );
        }
        assert_eq!(
            watch.masters.values().next().unwrap().drift_per_second(),
            None
        );
    }

    /// The peer-delay responder's two stamps are both its own, so the
    /// subtraction is valid with no shared time at all.
    #[test]
    fn peer_delay_turnaround_comes_from_the_responders_own_two_stamps() {
        let mut watch = OffsetWatch::default();
        let at = Instant::now();
        watch.observe(
            &message(0x3, 12, true, Some((500, 1_000)), 0),
            wall(1_000, 0),
            at,
        );
        watch.observe(
            &message(0xA, 12, true, Some((500, 6_000)), 0),
            wall(1_000, 0),
            at,
        );
        let delay = watch.delays.values().next().unwrap();
        assert_eq!(delay.turnaround_samples, 1);
        assert_eq!(delay.mean_turnaround(), Some(5_000.0));
    }

    /// A one-step transmitter that cannot stamp on the wire sends zeroes.
    /// Read as an epoch time that is 1970, and the offset becomes decades.
    #[test]
    fn a_zero_timestamp_is_not_a_measurement() {
        let mut watch = OffsetWatch::default();
        watch.observe(
            &message(0x0, 1, false, None, 0),
            wall(1_000_000_000, 0),
            Instant::now(),
        );
        assert!(watch.masters.is_empty());
    }

    /// An orphan Follow_Up — capture started mid-stream — must not pair with
    /// nothing and must not panic.
    #[test]
    fn an_orphan_follow_up_produces_no_sample() {
        let mut watch = OffsetWatch::default();
        watch.observe(
            &message(0x8, 400, true, Some((1_000_000_037, 0)), 0),
            wall(1_000_000_000, 0),
            Instant::now(),
        );
        assert!(watch.masters.is_empty());
    }
}
