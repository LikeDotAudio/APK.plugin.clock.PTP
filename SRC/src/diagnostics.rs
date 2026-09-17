// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! Clock and synchronization diagnostics — the three measurements that
//! separate a network that carries audio from one that drops it.

pub mod arrival;
pub mod election;
pub mod offset;

use crate::flow::PortKey;
use crate::message::{MessageType, PtpMessage};
use crate::monitor::Meta;
use std::time::Instant;

pub use arrival::{ArrivalWatch, Cadence, StreamKey};
pub use election::{DomainElection, ElectionWatch, GrandmasterClaim};
pub use offset::{MasterOffset, OffsetWatch, PathDelay};

/// Fraction of a stream that may go missing before it is called lossy.
///
/// One percent. A Sync stream at 8/s losing 1% loses a message every twelve
/// seconds, which a servo rides through; losing much more than that is where
/// a slave starts extrapolating and where the drift becomes audible on a
/// long cable run.
const LOSS_LIMIT: f64 = 0.01;

/// Peak arrival swing, as a fraction of the stream's own advertised interval,
/// above which the spacing is called jittery.
///
/// Twenty percent. Expressed as a fraction rather than as nanoseconds because
/// the same absolute swing is nothing at a 1s Announce rate and severe at a
/// 125ms gPTP Sync rate, and a fixed threshold would be wrong at one end or
/// the other however it were chosen.
const VARIATION_LIMIT: f64 = 0.20;

/// Messages needed before any verdict but `Unknown` is given.
///
/// A verdict from three packets is a guess wearing a verdict's clothes, and
/// the whole value of this lane is that a green reading can be trusted.
const MINIMUM_SAMPLES: u64 = 8;

/// What the diagnostics conclude about one port.
///
/// This is a CONCLUSION and it is ordered by how much it should worry the
/// person reading it — which is not the order the measurements were taken in.
/// A domain with two grandmasters in it is a worse problem than a jittery
/// Sync, even though the jitter is the number that moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough has been heard to say anything.
    Unknown,
    /// Rate, spacing and loss are all within limits and the master is locked.
    Stable,
    /// The spacing is coming apart.
    Jittery,
    /// Messages are going missing.
    Lossy,
    /// The grandmaster is announcing, but is no longer disciplined by its
    /// reference.
    Holdover,
    /// The elected grandmaster changed during this capture.
    Unstable,
    /// More than one grandmaster is live in this domain. Devices on either
    /// side of the split are not on the same time.
    Contested,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stable => "stable",
            Self::Jittery => "jittery",
            Self::Lossy => "lossy",
            Self::Holdover => "holdover",
            Self::Unstable => "unstable",
            Self::Contested => "contested",
        }
    }

    /// One line saying what this verdict means for audio on this network.
    pub fn meaning(&self) -> &'static str {
        match self {
            Self::Unknown => "not enough traffic heard to judge",
            Self::Stable => "locked, on rate, nothing lost",
            Self::Jittery => "arrival spacing is coming apart",
            Self::Lossy => "sync messages are going missing",
            Self::Holdover => "grandmaster is free-running, time is no longer traceable",
            Self::Unstable => "the grandmaster changed during this capture",
            Self::Contested => "two grandmasters in one domain — devices are not on the same time",
        }
    }
}

/// Sums that only ever rise while the ports behind them live.
///
/// Retiring a port takes its share away, so a consumer differencing two of
/// these must floor the result at zero rather than trust the subtraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub sync_lost: u64,
    pub sync_reordered: u64,
    pub reelections: u64,
    pub class_changes: u64,
}

/// The three watches, fed from one message stream.
///
/// They are kept together because every verdict needs more than one of them:
/// a jittery Sync during an election is an election, not a jittery Sync, and
/// only a reader holding both can say so.
#[derive(Default)]
pub struct Diagnostics {
    pub arrival: ArrivalWatch,
    pub offset: OffsetWatch,
    pub election: ElectionWatch,
}

impl Diagnostics {
    pub fn observe(&mut self, message: &PtpMessage, meta: &Meta) {
        self.arrival.observe(message, meta.at);
        self.offset.observe(message, meta.wall, meta.at);
        self.election.observe(message, meta.at);
    }

    /// Re-run the elections against the clock, with no message to drive it.
    ///
    /// A master going silent is the one state change that no packet announces,
    /// so a capture that has fallen quiet still has to be swept or it reports
    /// the dead master as the current winner forever.
    pub fn settle(&mut self, now: Instant) {
        self.election.settle(now);
    }

    /// Forget every port the caller no longer considers live.
    pub fn retain_ports(&mut self, keep: impl Fn(&PortKey) -> bool + Copy) {
        self.arrival.retain_ports(keep);
        self.offset.retain_ports(keep);
    }

    /// The monotonic totals a counter publisher differences between samples.
    ///
    /// Here rather than in the agent because every one of them is a sum over
    /// structures this module owns, and the two subtleties — that a domain's
    /// FIRST election is not a re-election, and that a retired port lowers a
    /// total that is supposed to only rise — are properties of the
    /// measurement, not of the publisher.
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for (key, cadence) in &self.arrival.streams {
            if key.message_type == MessageType::Sync {
                totals.sync_lost += cadence.lost;
                totals.sync_reordered += cadence.reordered;
            }
        }
        for election in self.election.domains.values() {
            // The first election in a domain is the domain appearing, not a
            // master being replaced. Counted, it would report a clean network
            // as having re-elected once per domain at startup.
            totals.reelections += election.changes.saturating_sub(1);
            for claim in election.claims.values() {
                totals.class_changes += claim.class_changes;
            }
        }
        totals
    }

    /// What this port's synchronization looks like, as one word.
    ///
    /// Ordered worst-first: the first condition that holds is the answer, so a
    /// contested domain is never reported as merely jittery.
    pub fn verdict(&self, key: &PortKey, grandmaster: Option<[u8; 8]>) -> Verdict {
        if let Some(domain) = self.election.domain(key.domain, key.variant) {
            if domain.unresolved() {
                return Verdict::Contested;
            }
            if domain.changes > 1 {
                return Verdict::Unstable;
            }
        }
        if let Some(identity) = grandmaster
            && self
                .election
                .claim(key.domain, key.variant, &identity)
                .is_some_and(|claim| claim.in_holdover())
        {
            return Verdict::Holdover;
        }

        let Some(sync) = self.arrival.stream(key, MessageType::Sync) else {
            return Verdict::Unknown;
        };
        if sync.count < MINIMUM_SAMPLES {
            return Verdict::Unknown;
        }
        if sync.loss_ratio().is_some_and(|ratio| ratio > LOSS_LIMIT) {
            return Verdict::Lossy;
        }
        if let Some(interval) = sync.expected_interval
            && interval > 0.0
            && sync.variation_peak / interval > VARIATION_LIMIT
        {
            return Verdict::Jittery;
        }
        Verdict::Stable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{parse, Variant};
    use crate::monitor::Meta;
    use std::time::{Duration, UNIX_EPOCH};

    const GM: [u8; 8] = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];
    const OTHER: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xFF, 0xFE, 0x00, 0x00, 0x01];

    fn meta(at: Instant) -> Meta {
        Meta {
            source: "44.44.44.154".into(),
            port: Some(319),
            interface: "udp".into(),
            at,
            wall: UNIX_EPOCH + Duration::from_secs(1_000_000_000),
        }
    }

    fn sync(clock: [u8; 8], sequence: u16) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = 0x0;
        payload[1] = 0x02;
        payload[20..28].copy_from_slice(&clock);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        payload[30..32].copy_from_slice(&sequence.to_be_bytes());
        payload[33] = 0xFD; // 125ms
        payload.extend_from_slice(&[0u8; 10]);
        parse(&payload, Variant::V2Udp).unwrap()
    }

    fn announce(clock: [u8; 8], grandmaster: [u8; 8], class: u8) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = 0xB;
        payload[1] = 0x02;
        payload[20..28].copy_from_slice(&clock);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        let mut body = vec![0u8; 30];
        body[13] = 128;
        body[14] = class;
        body[18] = 128;
        body[19..27].copy_from_slice(&grandmaster);
        payload.extend_from_slice(&body);
        parse(&payload, Variant::V2Udp).unwrap()
    }

    fn steady(diagnostics: &mut Diagnostics, start: Instant, count: u16, gap: f64) {
        for step in 0..count {
            let at = start + Duration::from_secs_f64(gap * f64::from(step));
            diagnostics.observe(&sync(GM, step), &meta(at));
        }
    }

    /// A clean Sync stream with an Announce beside it once a second, which is
    /// the shape a real master emits. Without the Announces the masters time
    /// out mid-test and every verdict becomes `Unstable` — correctly, which is
    /// exactly why the fixture has to keep announcing.
    fn steady_announced(
        diagnostics: &mut Diagnostics,
        start: Instant,
        count: u16,
        masters: &[([u8; 8], u8)],
    ) {
        for step in 0..count {
            let at = start + Duration::from_secs_f64(0.125 * f64::from(step));
            if step % 8 == 0 {
                for (grandmaster, class) in masters {
                    diagnostics.observe(&announce(*grandmaster, *grandmaster, *class), &meta(at));
                }
            }
            diagnostics.observe(&sync(GM, step), &meta(at));
        }
    }

    #[test]
    fn a_clean_stream_reads_as_stable() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady(&mut diagnostics, start, 40, 0.125);
        let key = PortKey::of(&sync(GM, 0));
        assert_eq!(diagnostics.verdict(&key, None), Verdict::Stable);
    }

    #[test]
    fn too_little_traffic_is_unknown_not_stable() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady(&mut diagnostics, start, 3, 0.125);
        let key = PortKey::of(&sync(GM, 0));
        assert_eq!(diagnostics.verdict(&key, None), Verdict::Unknown);
    }

    #[test]
    fn a_gapped_stream_reads_as_lossy() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        let mut at = start;
        // Every fourth Sync is missing.
        for step in 0..40u16 {
            if step % 4 != 0 {
                diagnostics.observe(&sync(GM, step), &meta(at));
            }
            at += Duration::from_secs_f64(0.125);
        }
        let key = PortKey::of(&sync(GM, 0));
        assert_eq!(diagnostics.verdict(&key, None), Verdict::Lossy);
    }

    #[test]
    fn a_stream_that_stops_and_starts_reads_as_jittery() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        let mut at = start;
        for step in 0..40u16 {
            diagnostics.observe(&sync(GM, step), &meta(at));
            at += Duration::from_secs_f64(if step == 20 { 0.400 } else { 0.125 });
        }
        let key = PortKey::of(&sync(GM, 0));
        assert_eq!(diagnostics.verdict(&key, None), Verdict::Jittery);
    }

    /// Holdover outranks jitter: a free-running grandmaster is the diagnosis
    /// even when the packets are arriving beautifully.
    #[test]
    fn holdover_outranks_a_clean_stream() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady_announced(&mut diagnostics, start, 40, &[(GM, 6)]);
        let key = PortKey::of(&sync(GM, 0));
        assert_eq!(diagnostics.verdict(&key, Some(GM)), Verdict::Stable);

        // The same master, still announcing, no longer locked to its reference.
        diagnostics.observe(&announce(GM, GM, 7), &meta(start + Duration::from_secs_f64(5.0)));
        assert_eq!(diagnostics.verdict(&key, Some(GM)), Verdict::Holdover);
    }

    /// Two grandmasters in one domain outranks everything else: nothing else
    /// on the list stops two halves of a network exchanging audio.
    #[test]
    fn a_contested_domain_outranks_every_other_verdict() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady_announced(&mut diagnostics, start, 40, &[(GM, 6), (OTHER, 6)]);
        let key = PortKey::of(&sync(GM, 0));
        let election = diagnostics.election.domain(0, Variant::V2Udp).unwrap();
        assert_eq!(election.elected, Some(GM), "identity is the final tiebreak");
        assert_eq!(diagnostics.verdict(&key, Some(GM)), Verdict::Contested);
    }

    /// A dead master must not stay elected just because nothing is arriving to
    /// notice. The sweep is what makes silence a state change.
    #[test]
    fn settling_expires_a_master_that_stopped_announcing() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        diagnostics.observe(&announce(GM, GM, 6), &meta(start));
        assert!(diagnostics.election.domain(0, Variant::V2Udp).unwrap().elected.is_some());

        diagnostics.settle(start + Duration::from_secs(30));
        let domain = diagnostics.election.domain(0, Variant::V2Udp).unwrap();
        assert_eq!(domain.elected, None);
        assert_eq!(domain.history.back().unwrap().reason, election::Reason::MasterLost);
    }

    /// A domain appearing is not a master being replaced. Counted as one, a
    /// perfectly healthy network reports a re-election per domain at startup.
    #[test]
    fn the_first_election_in_a_domain_is_not_a_reelection() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady_announced(&mut diagnostics, start, 40, &[(GM, 6)]);
        assert_eq!(diagnostics.totals().reelections, 0);
        assert_eq!(diagnostics.totals().class_changes, 0);

        // The master falls into holdover, then a second one takes the domain.
        diagnostics.observe(&announce(GM, GM, 248), &meta(start + Duration::from_secs_f64(5.0)));
        assert_eq!(diagnostics.totals().class_changes, 1);
        diagnostics.observe(&announce(OTHER, OTHER, 6), &meta(start + Duration::from_secs_f64(5.1)));
        assert_eq!(diagnostics.totals().reelections, 1);
    }

    /// A port that goes away must take its measurements with it, or a long
    /// capture accumulates a stream per clock that ever appeared.
    #[test]
    fn retiring_a_port_forgets_its_measurements() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        steady(&mut diagnostics, start, 10, 0.125);
        assert!(!diagnostics.arrival.streams.is_empty());
        diagnostics.retain_ports(|_| false);
        assert!(diagnostics.arrival.streams.is_empty());
        assert!(diagnostics.offset.masters.is_empty());
    }
}
