// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! Best Master Clock election state, and whether the winner is still locked.

use crate::flow::PortKey;
use crate::message::{ClockQuality, MessageType, PtpMessage, Variant};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Announces missed before a master is considered gone (§9.2.6.11 —
/// `announceReceiptTimeout`, whose default is 3).
const ANNOUNCE_RECEIPT_TIMEOUT: f64 = 3.0;

/// The interval assumed when the advertised one is not usable. Two seconds is
/// the 1588 default; gPTP announces faster, which only makes the timeout
/// derived from it more forgiving rather than less.
const DEFAULT_ANNOUNCE_INTERVAL: f64 = 2.0;

/// The band a `logAnnounceInterval` has to fall in to be believed: 2^-5 to
/// 2^4 seconds, which covers every profile in use here with room either side.
const SHORTEST_ANNOUNCE_INTERVAL: f64 = 0.031_25;
const LONGEST_ANNOUNCE_INTERVAL: f64 = 16.0;

/// The advertised announce interval, or the default when it cannot be true.
///
/// `logMessageInterval` is a signed power of two and nothing on the wire stops
/// a device sending 127. Taken at face value that is one announce every 5e30
/// years, and the receipt timeout derived from it means a grandmaster that
/// died stays elected for the life of the process — the exact failure this
/// whole module exists to catch, caused by the module itself.
fn believable_announce_interval(advertised: f64) -> f64 {
    if advertised.is_finite()
        && (SHORTEST_ANNOUNCE_INTERVAL..=LONGEST_ANNOUNCE_INTERVAL).contains(&advertised)
    {
        advertised
    } else {
        DEFAULT_ANNOUNCE_INTERVAL
    }
}

/// Elections kept per domain. Enough to see a flapping master; bounded so a
/// pathological network cannot grow the record without limit.
const HISTORY_DEPTH: usize = 32;

/// `clockClass` values that mean the master is no longer disciplined by its
/// reference (§7.6.2.5). This is the transition behind most drift complaints:
/// nothing on the wire stops, the announcements keep coming, and the time they
/// carry quietly stops being traceable to anything.
pub fn is_holdover(class: u8) -> bool {
    matches!(class, 7 | 14 | 52 | 58 | 187 | 193 | 248)
}

/// `clockClass` values that mean the master IS disciplined by its reference.
pub fn is_locked(class: u8) -> bool {
    matches!(class, 6 | 13)
}

/// One grandmaster as announced, plus how its quality has moved since.
#[derive(Debug, Clone)]
pub struct GrandmasterClaim {
    pub identity: [u8; 8],
    pub priority1: u8,
    pub quality: ClockQuality,
    pub priority2: u8,
    pub steps_removed: u16,
    pub time_source: u8,
    /// The port whose Announce most recently carried this claim.
    pub announced_by: PortKey,
    pub announce_interval: f64,
    pub first_at: Instant,
    pub last_at: Instant,
    /// How many times `clockClass` has changed under this identity.
    pub class_changes: u64,
    /// Set the moment the class first says holdover, cleared when it locks
    /// again. A master that recovers should not keep reading as degraded.
    pub holdover_since: Option<Instant>,
    /// The best class this identity has ever announced. A master now at 248
    /// that was at 6 an hour ago is a fault; one that has only ever been at
    /// 248 is a device without a reference, which is not the same story.
    pub best_class: u8,
}

impl GrandmasterClaim {
    /// The BMCA data set comparison (§9.3.4), as the ordering it really is.
    ///
    /// Lower wins, field by field, and the clock identity is the final
    /// tiebreak — which is what makes the algorithm deterministic and is why
    /// two devices with identical settings still converge instead of flapping.
    fn ranking(&self) -> (u8, u8, u8, u16, u8, [u8; 8]) {
        (
            self.priority1,
            self.quality.class,
            self.quality.accuracy,
            self.quality.offset_scaled_log_variance,
            self.priority2,
            self.identity,
        )
    }

    pub fn wins_over(&self, other: &Self) -> bool {
        self.ranking() < other.ranking()
    }

    /// How long this claim may go unheard before BMCA would drop it.
    fn silence_budget(&self) -> Duration {
        Duration::from_secs_f64(self.announce_interval * ANNOUNCE_RECEIPT_TIMEOUT)
    }

    pub fn in_holdover(&self) -> bool {
        is_holdover(self.quality.class)
    }

    /// True when this master used to be locked and is not any more.
    pub fn has_fallen_out_of_lock(&self) -> bool {
        self.in_holdover() && is_locked(self.best_class)
    }
}

/// Why the elected master changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The first master heard in this domain.
    FirstHeard,
    /// A better data set arrived and BMCA prefers it.
    BetterClock,
    /// The previous winner stopped announcing and timed out.
    MasterLost,
}

impl Reason {
    pub fn label(&self) -> &'static str {
        match self {
            Self::FirstHeard => "first heard",
            Self::BetterClock => "better clock",
            Self::MasterLost => "master lost",
        }
    }
}

/// One recorded change of the elected master.
#[derive(Debug, Clone, Copy)]
pub struct ElectionChange {
    pub from: Option<[u8; 8]>,
    pub to: Option<[u8; 8]>,
    pub at: Instant,
    pub reason: Reason,
}

/// The election in one domain of one PTP variant.
///
/// A domain is the scope of an election, and the variant is part of it because
/// a device running PTPv2 and gPTP on one NIC is two logical clocks that may
/// elect differently. Merged, a perfectly healthy pair of independent
/// elections reads as one domain that cannot make up its mind.
pub struct DomainElection {
    pub domain: u8,
    pub variant: Variant,
    pub claims: HashMap<[u8; 8], GrandmasterClaim>,
    pub elected: Option<[u8; 8]>,
    pub elected_since: Instant,
    pub changes: u64,
    pub history: VecDeque<ElectionChange>,
}

impl DomainElection {
    fn new(domain: u8, variant: Variant, at: Instant) -> Self {
        Self {
            domain,
            variant,
            claims: HashMap::new(),
            elected: None,
            elected_since: at,
            changes: 0,
            history: VecDeque::new(),
        }
    }

    /// The winning claim, if one is still being heard.
    pub fn winner(&self) -> Option<&GrandmasterClaim> {
        self.elected.and_then(|identity| self.claims.get(&identity))
    }

    /// More than one master still announcing in this domain.
    ///
    /// BMCA is meant to converge on one. Two live claims is either an election
    /// still running — which lasts seconds, not minutes — or a network split
    /// where each half elected its own. Devices on opposite sides are not on
    /// the same time and cannot exchange audio, which makes this the single
    /// most useful line in the report when it is true.
    pub fn unresolved(&self) -> bool {
        self.claims.len() > 1
    }

    /// How long the current winner has held the domain.
    pub fn held_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.elected_since)
    }

    fn observe(&mut self, message: &PtpMessage, at: Instant) {
        let Some(announce) = &message.announce else {
            return;
        };
        let interval = believable_announce_interval(message.interval_secs());
        let entry = self
            .claims
            .entry(announce.grandmaster_identity)
            .or_insert_with(|| GrandmasterClaim {
                identity: announce.grandmaster_identity,
                priority1: announce.grandmaster_priority1,
                quality: announce.grandmaster_quality,
                priority2: announce.grandmaster_priority2,
                steps_removed: announce.steps_removed,
                time_source: announce.time_source,
                announced_by: PortKey::of(message),
                announce_interval: interval,
                first_at: at,
                last_at: at,
                class_changes: 0,
                holdover_since: None,
                best_class: announce.grandmaster_quality.class,
            });

        if entry.quality.class != announce.grandmaster_quality.class {
            entry.class_changes += 1;
        }
        // Lower class is better, and 0 is reserved rather than excellent, so
        // "best" is the smallest non-zero one ever announced.
        if announce.grandmaster_quality.class != 0
            && (entry.best_class == 0 || announce.grandmaster_quality.class < entry.best_class)
        {
            entry.best_class = announce.grandmaster_quality.class;
        }
        entry.holdover_since = match (
            is_holdover(announce.grandmaster_quality.class),
            entry.holdover_since,
        ) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(at),
            (false, _) => None,
        };
        entry.priority1 = announce.grandmaster_priority1;
        entry.quality = announce.grandmaster_quality;
        entry.priority2 = announce.grandmaster_priority2;
        entry.steps_removed = announce.steps_removed;
        entry.time_source = announce.time_source;
        entry.announced_by = PortKey::of(message);
        entry.announce_interval = interval;
        entry.last_at = at;
    }

    /// Drop claims that have gone past their announce receipt timeout.
    fn expire(&mut self, now: Instant) -> bool {
        let before = self.claims.len();
        self.claims.retain(|_, claim| {
            now.saturating_duration_since(claim.last_at) < claim.silence_budget()
        });
        before != self.claims.len()
    }

    /// Re-run the election and record it if the winner moved.
    fn settle(&mut self, now: Instant, lost_a_claim: bool) {
        let winner = self
            .claims
            .values()
            .reduce(|best, claim| if claim.wins_over(best) { claim } else { best })
            .map(|claim| claim.identity);

        if winner == self.elected {
            return;
        }
        let reason = match (self.elected, winner) {
            (None, Some(_)) => Reason::FirstHeard,
            _ if lost_a_claim => Reason::MasterLost,
            _ => Reason::BetterClock,
        };
        let change = ElectionChange {
            from: self.elected,
            to: winner,
            at: now,
            reason,
        };
        self.elected = winner;
        self.elected_since = now;
        self.changes += 1;
        if self.history.len() == HISTORY_DEPTH {
            self.history.pop_front();
        }
        self.history.push_back(change);
    }
}

/// Every domain's election, across every variant on the wire.
#[derive(Default)]
pub struct ElectionWatch {
    pub domains: HashMap<(u8, Variant), DomainElection>,
}

impl ElectionWatch {
    /// Feed one message. Only Announce carries an election; everything else
    /// still drives the expiry sweep, because a master going silent is a
    /// change that no message announces.
    pub fn observe(&mut self, message: &PtpMessage, at: Instant) {
        if message.message_type == MessageType::Announce {
            self.domains
                .entry((message.domain, message.variant))
                .or_insert_with(|| DomainElection::new(message.domain, message.variant, at))
                .observe(message, at);
        }
        self.settle(at);
    }

    /// Re-run every domain's election against the clock.
    pub fn settle(&mut self, now: Instant) {
        for election in self.domains.values_mut() {
            let lost = election.expire(now);
            election.settle(now, lost);
        }
    }

    /// The election covering a port, if that port's domain has one.
    pub fn domain(&self, domain: u8, variant: Variant) -> Option<&DomainElection> {
        self.domains.get(&(domain, variant))
    }

    /// The claim a given grandmaster identity is making, wherever it is.
    pub fn claim(
        &self,
        domain: u8,
        variant: Variant,
        identity: &[u8; 8],
    ) -> Option<&GrandmasterClaim> {
        self.domains.get(&(domain, variant))?.claims.get(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::parse;

    const BETTER: [u8; 8] = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];
    const WORSE: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xFF, 0xFE, 0x00, 0x00, 0x01];

    #[allow(clippy::too_many_arguments)]
    fn announce(
        announcer: [u8; 8],
        grandmaster: [u8; 8],
        priority1: u8,
        class: u8,
        priority2: u8,
        domain: u8,
        log_interval: i8,
    ) -> PtpMessage {
        let mut payload = vec![0u8; 34];
        payload[0] = 0xB;
        payload[1] = 0x02;
        payload[4] = domain;
        payload[20..28].copy_from_slice(&announcer);
        payload[28..30].copy_from_slice(&1u16.to_be_bytes());
        payload[33] = log_interval as u8;
        let mut body = vec![0u8; 30];
        body[13] = priority1;
        body[14] = class;
        body[15] = 0x21;
        body[18] = priority2;
        body[19..27].copy_from_slice(&grandmaster);
        payload.extend_from_slice(&body);
        parse(&payload, Variant::V2Udp).unwrap()
    }

    #[test]
    fn the_first_master_heard_wins_the_domain() {
        let mut watch = ElectionWatch::default();
        let at = Instant::now();
        watch.observe(&announce(BETTER, BETTER, 128, 6, 128, 0, 1), at);
        let election = watch.domain(0, Variant::V2Udp).unwrap();
        assert_eq!(election.elected, Some(BETTER));
        assert_eq!(election.changes, 1);
        assert_eq!(election.history.back().unwrap().reason, Reason::FirstHeard);
    }

    /// The whole point of BMCA: priority1 beats everything below it, so the
    /// election is decided by configuration and not by who spoke first.
    #[test]
    fn a_lower_priority1_takes_the_domain() {
        let mut watch = ElectionWatch::default();
        let at = Instant::now();
        watch.observe(&announce(WORSE, WORSE, 128, 6, 128, 0, 1), at);
        watch.observe(&announce(BETTER, BETTER, 100, 248, 128, 0, 1), at);
        let election = watch.domain(0, Variant::V2Udp).unwrap();
        assert_eq!(
            election.elected,
            Some(BETTER),
            "priority1 outranks clockClass, however bad the class"
        );
        assert_eq!(election.changes, 2);
        assert_eq!(election.history.back().unwrap().reason, Reason::BetterClock);
    }

    /// Two masters announcing at once is either an election in progress or a
    /// split network, and the report has to be able to say so.
    #[test]
    fn two_live_masters_read_as_unresolved() {
        let mut watch = ElectionWatch::default();
        let at = Instant::now();
        watch.observe(&announce(WORSE, WORSE, 128, 6, 128, 0, 1), at);
        watch.observe(&announce(BETTER, BETTER, 128, 6, 128, 0, 1), at);
        let election = watch.domain(0, Variant::V2Udp).unwrap();
        assert!(election.unresolved());
        // Identity is the final tiebreak, and BETTER sorts first.
        assert_eq!(election.elected, Some(BETTER));
    }

    /// A master that stops announcing must lose the domain to the runner-up
    /// after its announce receipt timeout, with the reason recorded.
    #[test]
    fn a_silent_master_times_out_and_hands_over() {
        let mut watch = ElectionWatch::default();
        let start = Instant::now();
        // logMessageInterval 0 -> 1s announces -> a 3s receipt timeout.
        watch.observe(&announce(BETTER, BETTER, 100, 6, 128, 0, 0), start);
        watch.observe(&announce(WORSE, WORSE, 128, 6, 128, 0, 0), start);
        assert_eq!(
            watch.domain(0, Variant::V2Udp).unwrap().elected,
            Some(BETTER)
        );

        // Only the runner-up keeps announcing.
        watch.observe(
            &announce(WORSE, WORSE, 128, 6, 128, 0, 0),
            start + Duration::from_secs(4),
        );
        let election = watch.domain(0, Variant::V2Udp).unwrap();
        assert_eq!(election.elected, Some(WORSE));
        assert_eq!(election.history.back().unwrap().reason, Reason::MasterLost);
        assert!(!election.unresolved());
    }

    /// Falling into holdover is the failure that changes nothing visible: the
    /// announcements keep coming and the time in them stops being traceable.
    #[test]
    fn a_master_falling_into_holdover_is_recorded() {
        let mut watch = ElectionWatch::default();
        let start = Instant::now();
        watch.observe(&announce(BETTER, BETTER, 128, 6, 128, 0, 0), start);
        let claim = watch.claim(0, Variant::V2Udp, &BETTER).unwrap();
        assert!(!claim.in_holdover());

        watch.observe(
            &announce(BETTER, BETTER, 128, 7, 128, 0, 0),
            start + Duration::from_secs(1),
        );
        let claim = watch.claim(0, Variant::V2Udp, &BETTER).unwrap();
        assert!(claim.in_holdover());
        assert!(
            claim.has_fallen_out_of_lock(),
            "it was locked to GPS a second ago"
        );
        assert_eq!(claim.class_changes, 1);
        assert_eq!(claim.best_class, 6);
    }

    /// A device that has never had a reference is not a device that lost one.
    #[test]
    fn a_permanently_free_running_clock_has_not_fallen_out_of_lock() {
        let mut watch = ElectionWatch::default();
        watch.observe(&announce(WORSE, WORSE, 128, 248, 128, 0, 0), Instant::now());
        let claim = watch.claim(0, Variant::V2Udp, &WORSE).unwrap();
        assert!(claim.in_holdover());
        assert!(!claim.has_fallen_out_of_lock());
    }

    /// Recovery must clear the flag, or a master that came back keeps reading
    /// as degraded for the rest of the capture.
    #[test]
    fn locking_again_clears_the_holdover_stamp() {
        let mut watch = ElectionWatch::default();
        let start = Instant::now();
        watch.observe(&announce(BETTER, BETTER, 128, 7, 128, 0, 0), start);
        assert!(
            watch
                .claim(0, Variant::V2Udp, &BETTER)
                .unwrap()
                .holdover_since
                .is_some()
        );
        watch.observe(
            &announce(BETTER, BETTER, 128, 6, 128, 0, 0),
            start + Duration::from_secs(1),
        );
        assert!(
            watch
                .claim(0, Variant::V2Udp, &BETTER)
                .unwrap()
                .holdover_since
                .is_none()
        );
    }

    /// Two domains are two elections, and a master in one must not win the
    /// other.
    #[test]
    fn domains_elect_separately() {
        let mut watch = ElectionWatch::default();
        let at = Instant::now();
        watch.observe(&announce(BETTER, BETTER, 128, 6, 128, 0, 0), at);
        watch.observe(&announce(WORSE, WORSE, 128, 6, 128, 127, 0), at);
        assert_eq!(
            watch.domain(0, Variant::V2Udp).unwrap().elected,
            Some(BETTER)
        );
        assert_eq!(
            watch.domain(127, Variant::V2Udp).unwrap().elected,
            Some(WORSE)
        );
    }

    /// A `logAnnounceInterval` of 127 is one announce every 5e30 years. Taken
    /// at face value the receipt timeout is infinite and a dead grandmaster
    /// stays elected forever — which is the failure this module is for.
    #[test]
    fn an_impossible_announce_interval_falls_back_to_the_default() {
        let mut watch = ElectionWatch::default();
        let start = Instant::now();
        watch.observe(&announce(BETTER, BETTER, 128, 6, 128, 0, 127), start);
        assert_eq!(
            watch
                .claim(0, Variant::V2Udp, &BETTER)
                .unwrap()
                .announce_interval,
            DEFAULT_ANNOUNCE_INTERVAL
        );

        // 3 x 2s, so it is gone by 7 seconds of silence.
        watch.settle(start + Duration::from_secs(7));
        assert_eq!(watch.domain(0, Variant::V2Udp).unwrap().elected, None);
    }

    /// A flapping master must not grow the record without bound.
    #[test]
    fn the_change_history_is_bounded() {
        let mut watch = ElectionWatch::default();
        let start = Instant::now();
        for step in 0..100u64 {
            let (announcer, grandmaster) = if step % 2 == 0 {
                (BETTER, BETTER)
            } else {
                (WORSE, WORSE)
            };
            // Each pass is far enough apart that the other master times out.
            watch.observe(
                &announce(announcer, grandmaster, 128, 6, 128, 0, 0),
                start + Duration::from_secs(step * 5),
            );
        }
        let election = watch.domain(0, Variant::V2Udp).unwrap();
        assert!(election.changes > 10, "it really did flap");
        assert_eq!(election.history.len(), HISTORY_DEPTH);
    }
}
