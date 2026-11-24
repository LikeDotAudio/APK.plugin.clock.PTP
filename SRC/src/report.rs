// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! The capture, rendered as one JSON object.
//!
//! `ptp-monitor` is a tool you watch; this is the same run rendered for
//! something that reads. The machine reporter
//! (`APK:BareMetal/backend/interfaces_reporter.py`) shells out to
//! `ptp-monitor --json` and turns this straight into the *Clocks on the Wire*
//! block of the Interfaces → Clock tab, so the shape here IS the column order
//! there.
//!
//! Two things are deliberately reported rather than dropped:
//!
//! * `listening.problems` — a partial capture is a legitimate result, but only
//!   if it says which half is missing. "No gPTP clocks" and "could not listen
//!   for gPTP" must never render the same.
//! * `counts.udp_blocked` — the conclusion, not just the raw split. Raw
//!   AF_PACKET capture sits below netfilter and UDP does not, so UDP zero while
//!   Layer 2 flows is a host firewall, not an empty network.
//!
//! Hand-rolled rather than serde: this crate carries no serialisation
//! dependency, and one flat object is not worth acquiring one for.

use crate::diagnostics::Diagnostics;
use crate::message::{ClockQuality, MessageType, format_clock_id, time_source_meaning};
use crate::monitor::{ClockRecord, ClockTable, Transports};
use std::time::Instant;

/// What the capture loop counted, split by transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    pub total: u64,
    pub undecodable: u64,
    pub udp: u64,
    pub l2: u64,
}

/// JSON string literal, escaped.
fn s(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for c in raw.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `Some(v)` as a JSON value, `None` as `null` — never as `"-"`.
///
/// The dash belongs to the renderer. A consumer that gets `null` knows the
/// field was never announced; one that gets `"-"` has to guess whether that is
/// a missing value or a device that literally announced a dash.
fn json_or_null<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_text_or_null(v: Option<&str>) -> String {
    v.map(s).unwrap_or_else(|| "null".to_string())
}

/// A measurement as a JSON number, or `null` when there was not enough to
/// measure. Rounded here rather than by the reader, because the reader is a
/// table and a table cannot round without deciding how many digits mean
/// something — which is a question about the instrument, not about the layout.
fn number(value: Option<f64>, places: usize) -> String {
    value
        .filter(|v| v.is_finite())
        .map(|v| format!("{:.*}", places, v))
        .unwrap_or_else(|| "null".to_string())
}

/// The diagnostics for one clock: what its Sync stream looks like, how far
/// this host's clock sits from it, and what the fabric added on the way.
fn clock_diagnostics_json(diagnostics: &Diagnostics, record: &ClockRecord) -> String {
    let verdict = diagnostics.verdict_on(&record.key, record.grandmaster, Some(&record.interface));
    let sync = diagnostics.arrival.stream(&record.key, MessageType::Sync);
    let announce = diagnostics
        .arrival
        .stream(&record.key, MessageType::Announce);
    let offset = diagnostics.offset.master(&record.key);
    let delay = diagnostics.offset.delay(&record.key);

    format!(
        concat!(
            "{{\"verdict\":{},\"verdict_meaning\":{},",
            "\"sync_interval_mean_ms\":{},\"sync_jitter_ms\":{},",
            "\"sync_variation_peak_ms\":{},\"sync_rate_error\":{},",
            "\"sync_lost\":{},\"sync_reordered\":{},\"sync_seen\":{},",
            "\"announce_lost\":{},",
            "\"apparent_offset_ns\":{},\"offset_spread_ns\":{},",
            "\"offset_peak_to_peak_ns\":{},\"drift_ns_per_s\":{},",
            "\"offset_samples\":{},\"utc_offset_applied\":{},\"ptp_timescale\":{},",
            "\"correction_mean_ns\":{},\"correction_peak_ns\":{},",
            "\"peer_turnaround_mean_ns\":{}}}"
        ),
        s(verdict.label()),
        s(verdict.meaning()),
        number(sync.and_then(|c| c.mean_interval()).map(|v| v * 1000.0), 4),
        number(sync.and_then(|c| c.jitter()).map(|v| v * 1000.0), 4),
        number(sync.map(|c| c.variation_peak * 1000.0), 4),
        number(sync.and_then(|c| c.rate_error()), 6),
        sync.map(|c| c.lost).unwrap_or(0),
        sync.map(|c| c.reordered).unwrap_or(0),
        sync.map(|c| c.count).unwrap_or(0),
        announce.map(|c| c.lost).unwrap_or(0),
        json_or_null(offset.map(|o| o.latest_nanos())),
        number(offset.and_then(|o| o.spread_nanos()), 1),
        number(offset.and_then(|o| o.peak_to_peak_nanos()), 1),
        number(offset.and_then(|o| o.drift_per_second()), 3),
        offset.map(|o| o.samples).unwrap_or(0),
        offset.is_some_and(|o| o.utc_offset_applied),
        offset.is_some_and(|o| o.ptp_timescale),
        number(delay.and_then(|d| d.mean_correction()), 1),
        number(delay.map(|d| d.correction_peak), 1),
        number(delay.and_then(|d| d.mean_turnaround()), 1),
    )
}

/// Every domain's election, with the claims behind it.
///
/// Carried as its own block rather than folded into the clock rows because an
/// election is a property of a DOMAIN: repeated onto each of the twelve
/// devices in it, the one fact that two grandmasters are live reads as twelve
/// separate problems.
fn elections_json(diagnostics: &Diagnostics, now: Instant) -> String {
    let mut domains: Vec<_> = diagnostics.election.domains.values().collect();
    domains.sort_by_key(|d| (d.domain, d.variant.label(), d.interface.clone()));

    let blocks: Vec<String> = domains
        .iter()
        .map(|election| {
            let mut claims: Vec<_> = election.claims.values().collect();
            claims.sort_by_key(|claim| claim.identity);
            let claim_rows: Vec<String> = claims
                .iter()
                .map(|claim| {
                    let quality = claim.quality;
                    format!(
                        concat!(
                            "{{\"grandmaster\":{},\"priority1\":{},\"priority2\":{},",
                            "\"gm_class\":{},\"gm_class_meaning\":{},\"gm_accuracy\":{},",
                            "\"time_source\":{},\"steps_removed\":{},",
                            "\"announced_by\":{},\"announce_interval_s\":{},",
                            "\"in_holdover\":{},\"fell_out_of_lock\":{},",
                            "\"class_changes\":{},\"best_class\":{},\"heard_for_s\":{}}}"
                        ),
                        s(&format_clock_id(&claim.identity)),
                        claim.priority1,
                        claim.priority2,
                        quality.class,
                        s(quality.class_meaning()),
                        s(quality.accuracy_meaning()),
                        s(time_source_meaning(claim.time_source)),
                        claim.steps_removed,
                        s(&claim.announced_by.label()),
                        format!("{:.4}", claim.announce_interval),
                        claim.in_holdover(),
                        claim.has_fallen_out_of_lock(),
                        claim.class_changes,
                        claim.best_class,
                        format!(
                            "{:.3}",
                            claim
                                .last_at
                                .saturating_duration_since(claim.first_at)
                                .as_secs_f64()
                        ),
                    )
                })
                .collect();

            let history: Vec<String> = election
                .history
                .iter()
                .map(|change| {
                    format!(
                        "{{\"from\":{},\"to\":{},\"reason\":{},\"seconds_ago\":{:.3}}}",
                        json_text_or_null(change.from.map(|g| format_clock_id(&g)).as_deref()),
                        json_text_or_null(change.to.map(|g| format_clock_id(&g)).as_deref()),
                        s(change.reason.label()),
                        now.saturating_duration_since(change.at).as_secs_f64(),
                    )
                })
                .collect();

            format!(
                concat!(
                    "{{\"interface\":{},\"domain\":{},\"variant\":{},\"elected\":{},\"unresolved\":{},",
                    "\"changes\":{},\"held_for_s\":{:.3},\"claims\":[{}],\"history\":[{}]}}"
                ),
                s(&election.interface),
                election.domain,
                s(election.variant.label()),
                json_text_or_null(election.elected.map(|g| format_clock_id(&g)).as_deref()),
                election.unresolved(),
                election.changes,
                election.held_for(now).as_secs_f64(),
                claim_rows.join(","),
                history.join(","),
            )
        })
        .collect();
    blocks.join(",")
}

/// One JSON object describing everything this run saw.
pub fn snapshot_json(
    table: &ClockTable,
    diagnostics: &Diagnostics,
    transports: &Transports,
    counts: &Counters,
    capture_seconds: f64,
) -> String {
    let now = Instant::now();
    let interfaces: Vec<String> = transports
        .interfaces
        .iter()
        .map(|(n, _, _, _)| s(n))
        .collect();
    let problems: Vec<String> = transports.problems.iter().map(|p| s(p)).collect();

    // Same conclusion the MQTT agent publishes, drawn the same way.
    let udp_blocked = !transports.udp.is_empty() && counts.udp == 0 && counts.l2 > 0;

    let mut clocks: Vec<_> = table.clocks.values().collect();
    // Grandmasters first, then by identity — the important rows lead, and the
    // order is stable across runs so a diff of two snapshots means something.
    clocks.sort_by_key(|c| (c.steps_removed.unwrap_or(u16::MAX), c.key.label()));

    let rows: Vec<String> = clocks
        .iter()
        .map(|c| {
            let quality = ClockQuality {
                class: c.grandmaster_class.unwrap_or(0),
                accuracy: c.grandmaster_accuracy.unwrap_or(0xFE),
                offset_scaled_log_variance: 0,
            };
            format!(
                concat!(
                    "{{\"clock_id\":{},\"port\":{},\"role\":{},\"variant\":{},\"domain\":{},",
                    "\"interface\":{},\"source\":{},\"grandmaster\":{},\"gm_class\":{},",
                    "\"gm_class_meaning\":{},\"gm_accuracy\":{},\"time_source\":{},",
                    "\"steps_removed\":{},\"priority1\":{},\"priority2\":{},\"utc_offset\":{},",
                    "\"sync_interval_s\":{},\"announce_interval_s\":{},\"two_step\":{},",
                    "\"subdomain\":{},\"messages\":{},\"diagnostics\":{}}}"
                ),
                s(&format_clock_id(&c.key.clock_identity)),
                c.key.port_number,
                s(if c.is_grandmaster() {
                    "grandmaster"
                } else {
                    "clock"
                }),
                s(c.variant.label()),
                c.domain,
                s(&c.interface),
                s(&c.source),
                json_text_or_null(c.grandmaster.map(|g| format_clock_id(&g)).as_deref()),
                json_or_null(c.grandmaster_class),
                json_text_or_null(c.grandmaster_class.map(|_| quality.class_meaning())),
                json_text_or_null(c.grandmaster_accuracy.map(|_| quality.accuracy_meaning())),
                json_text_or_null(c.time_source.map(time_source_meaning)),
                json_or_null(c.steps_removed),
                json_or_null(c.priority1),
                json_or_null(c.priority2),
                json_or_null(c.utc_offset),
                json_or_null(c.sync_interval.map(|i| format!("{i:.6}"))),
                json_or_null(c.announce_interval.map(|i| format!("{i:.6}"))),
                c.two_step,
                json_text_or_null(c.subdomain.as_deref()),
                s(&c.message_summary()),
                clock_diagnostics_json(diagnostics, c),
            )
        })
        .collect();

    // More than one grandmaster in a domain means an unresolved BMCA election
    // or a split network. Carried as its own list so a reader does not have to
    // re-derive it by grouping the rows.
    let mut conflicts: Vec<String> = table
        .grandmasters_by_segment()
        .into_iter()
        .filter(|(_, gms)| gms.len() > 1)
        .map(|((interface, domain, variant), gms)| {
            let mut ids: Vec<String> = gms.iter().map(|g| s(&format_clock_id(g))).collect();
            ids.sort();
            format!(
                "{{\"interface\":{},\"domain\":{domain},\"variant\":{},\"grandmasters\":[{}]}}",
                s(&interface),
                s(variant.label()),
                ids.join(",")
            )
        })
        .collect();
    conflicts.sort();

    format!(
        concat!(
            "{{\"capture_seconds\":{:.3},",
            "\"listening\":{{\"udp_ports\":{},\"l2\":{},\"interfaces\":[{}],\"problems\":[{}]}},",
            "\"counts\":{{\"total\":{},\"undecodable\":{},\"udp\":{},\"l2\":{},\"udp_blocked\":{}}},",
            "\"clocks\":[{}],\"conflicts\":[{}],\"elections\":[{}]}}"
        ),
        capture_seconds,
        transports.udp.len(),
        transports.l2.is_some(),
        interfaces.join(","),
        problems.join(","),
        counts.total,
        counts.undecodable,
        counts.udp,
        counts.l2,
        udp_blocked,
        rows.join(","),
        conflicts.join(","),
        elections_json(diagnostics, now),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Variant, parse};
    use crate::monitor::Meta;
    use std::time::Instant;

    fn meta() -> Meta {
        Meta {
            source: "00:0A:92:01:56:A3".into(),
            port: None,
            interface: "enp5s0f0".into(),
            at: Instant::now(),
            wall: std::time::SystemTime::now(),
        }
    }

    fn announce(clock: [u8; 8], gm: [u8; 8], steps: u16) -> crate::message::PtpMessage {
        let mut p = vec![0u8; 34];
        p[0] = (1 << 4) | 0xB;
        p[1] = 0x02;
        p[20..28].copy_from_slice(&clock);
        p[28..30].copy_from_slice(&1u16.to_be_bytes());
        let mut body = vec![0u8; 30];
        body[14] = 6; // clockClass
        body[19..27].copy_from_slice(&gm);
        body[27..29].copy_from_slice(&steps.to_be_bytes());
        p.extend_from_slice(&body);
        parse(&p, Variant::V2Ethernet).unwrap()
    }

    /// The reporter parses this with `json.loads`, so it has to be JSON —
    /// including when there is nothing to report.
    #[test]
    fn an_empty_capture_is_still_valid_json() {
        let table = ClockTable::default();
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: vec!["nope \"quoted\"".into()],
        };
        let out = snapshot_json(
            &table,
            &Diagnostics::default(),
            &transports,
            &Counters::default(),
            5.0,
        );
        assert!(out.starts_with('{') && out.ends_with('}'));
        assert!(out.contains("\"clocks\":[]"));
        assert!(
            out.contains(r#"nope \"quoted\""#),
            "problems must be escaped: {out}"
        );
    }

    #[test]
    fn a_grandmaster_renders_its_announce_fields() {
        let gm = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];
        let mut table = ClockTable::default();
        table.record(&announce(gm, gm, 0), &meta());
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        let out = snapshot_json(
            &table,
            &Diagnostics::default(),
            &transports,
            &Counters::default(),
            5.0,
        );
        assert!(out.contains("\"role\":\"grandmaster\""));
        assert!(out.contains("\"gm_class\":6"));
        assert!(out.contains("\"interface\":\"enp5s0f0\""));
    }

    /// A field nobody announced must be `null`, not a dash — the renderer owns
    /// the dash, the capture owns the truth.
    #[test]
    fn unannounced_fields_are_null() {
        let gm = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];
        let mut table = ClockTable::default();
        table.record(&announce(gm, gm, 0), &meta());
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        let out = snapshot_json(
            &table,
            &Diagnostics::default(),
            &transports,
            &Counters::default(),
            5.0,
        );
        // No Sync was seen, so there is no sync interval to report.
        assert!(out.contains("\"sync_interval_s\":null"));
    }

    /// A populated capture, so the diagnostics block is exercised with real
    /// numbers in it rather than with the nulls an empty one produces.
    fn populated() -> (ClockTable, Diagnostics) {
        let gm = [0x00, 0x0A, 0x92, 0xFF, 0xFE, 0x01, 0x56, 0xA3];
        let mut table = ClockTable::default();
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        for step in 0..40u16 {
            let at = start + std::time::Duration::from_secs_f64(0.125 * f64::from(step));
            let meta = Meta {
                source: "00:0A:92:01:56:A3".into(),
                port: None,
                interface: "enp5s0f0".into(),
                at,
                wall: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            };
            let message = announce(gm, gm, 0);
            table.record(&message, &meta);
            diagnostics.observe(&message, &meta);
        }
        (table, diagnostics)
    }

    /// The reporter parses this with `json.loads`, and a single `NaN` takes the
    /// whole Clock tab down rather than one column. Every measurement here is a
    /// division that has a zero-sample case, so this is not hypothetical: it is
    /// what an empty capture produces if the guards are removed.
    #[test]
    fn no_measurement_ever_renders_as_nan_or_infinity() {
        let (table, diagnostics) = populated();
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        for out in [
            snapshot_json(&table, &diagnostics, &transports, &Counters::default(), 5.0),
            snapshot_json(
                &ClockTable::default(),
                &Diagnostics::default(),
                &transports,
                &Counters::default(),
                0.0,
            ),
        ] {
            for forbidden in ["NaN", "inf", "Infinity", ",,", ",}", ",]", "[,", "{,"] {
                assert!(!out.contains(forbidden), "{forbidden:?} in {out}");
            }
            assert_eq!(
                out.matches('{').count(),
                out.matches('}').count(),
                "unbalanced braces: {out}"
            );
            assert_eq!(out.matches('[').count(), out.matches(']').count());
        }
    }

    /// The verdict is the one field a dashboard reads without understanding
    /// PTP, so it has to be present on every row and never blank.
    #[test]
    fn every_clock_row_carries_a_verdict_and_its_meaning() {
        let (table, diagnostics) = populated();
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        let out = snapshot_json(&table, &diagnostics, &transports, &Counters::default(), 5.0);
        assert!(out.contains("\"diagnostics\":{\"verdict\":\""), "{out}");
        assert!(out.contains("\"verdict_meaning\":\""));
        assert!(out.contains("\"announce_lost\":0"));
    }

    /// An election is a property of a domain. Repeated onto every clock in it,
    /// one split network reads as a dozen separate problems.
    #[test]
    fn the_election_is_reported_once_per_domain_not_once_per_clock() {
        let (table, diagnostics) = populated();
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        let out = snapshot_json(&table, &diagnostics, &transports, &Counters::default(), 5.0);
        assert_eq!(out.matches("\"elected\":").count(), 1, "{out}");
        assert!(out.contains("\"unresolved\":false"));
        assert!(out.contains("\"reason\":\"first heard\""));
    }

    /// The firewall signature has to survive into the JSON, or the tab shows
    /// "no clocks" for a machine that is simply not being allowed to hear them.
    #[test]
    fn udp_blocked_is_concluded_not_left_to_the_reader() {
        let transports = Transports {
            udp: Vec::new(),
            l2: None,
            interfaces: Vec::new(),
            problems: Vec::new(),
        };
        let counts = Counters {
            total: 9,
            undecodable: 0,
            udp: 0,
            l2: 9,
        };
        // No UDP socket ever opened: silence on UDP is explained already.
        let out = snapshot_json(
            &ClockTable::default(),
            &Diagnostics::default(),
            &transports,
            &counts,
            5.0,
        );
        assert!(out.contains("\"udp_blocked\":false"));
    }
}
