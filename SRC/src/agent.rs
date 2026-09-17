// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! The MQTT agent: publish discovered clocks as retained topics.
//!
//! Publishing policy differs from every other discovery agent here, and for a
//! reason worth stating: PTP is *continuous, high-rate* traffic. gPTP Sync runs
//! at 8 messages a second per port by default, and a device announcing itself
//! over mDNS does so every few minutes. Republishing a retained topic per Sync
//! would put thousands of writes a minute on the bus to say nothing new.
//!
//! So the agent publishes on **state change and on a slow heartbeat**, not per
//! packet. The live packet-by-packet view is the `ptp-monitor` binary's job —
//! that is a tool you watch, not a state store.

use crate::diagnostics::{Diagnostics, Totals};
use crate::flow::FlowTracker;
use crate::message::{format_clock_id, time_source_meaning, ClockQuality};
use crate::monitor::{ClockTable, Observation, Transports};
use std::time::{Duration, Instant};
use apkaudio_contracts::agent_counters::Sampler;
use apkaudio_contracts::agent_state::AgentReport;
use apkaudio_contracts::device_publish::{Register, Sighting};
use apkaudio_contracts::device_record::DeviceRecordStatus;
use apkaudio_contracts::presence;
use apkaudio_contracts::topics::{sanitize_segment, TopicError};

/// Retained attribute keys per clock port. Order here is the UI column order.
///
/// `vendor` sits directly after `clock_id` because it is a reading OF that
/// column, not a fact beside it: the identity is a MAC with `FF:FE` inserted,
/// and its first three bytes are the manufacturer's IEEE block. A table of four
/// clocks called `00:07:F5:FF:FE:…` is complete and unreadable; the same table
/// with *Bridgeco Co AG* under each one is the same data.
///
/// It is published as its own key rather than folded into `clock_id`, because
/// `clock_id` is an identity — the topic segment is built from it and the
/// `grandmaster` column is matched against it. Decorating it would break both.
pub const CLOCK_KEYS: [&str; 22] = [
    "clock_id",
    "vendor",
    "port",
    "variant",
    "domain",
    "role",
    "grandmaster",
    "gm_class",
    "gm_class_meaning",
    "gm_accuracy",
    "time_source",
    "steps_removed",
    "priority1",
    "priority2",
    "sync_interval_s",
    "two_step",
    "subdomain",
    "messages",
    // A VERDICT, not a measurement, and that is the whole reason it may be
    // retained. Jitter, offset and drift move every heartbeat; publishing them
    // here would rewrite five retained topics per clock per 30 seconds to say
    // very nearly the same thing, which is the bus noise the publishing policy
    // at the top of this file exists to refuse. The numbers behind this word
    // are in `ptp-monitor --json`, which is a document a reader asks for.
    "sync_health",
    "status",
    // `-1` while the clock is announcing — see `apkaudio_contracts::presence`.
    "last_online",
    "online_since",
];

/// How often to refresh retained topics even when nothing changed.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// The same cadence as `HEARTBEAT`, declared in every `AgentCounters` payload
/// so a consumer can tell a missing sample from an idle agent. Derived from
/// the constant rather than typed again — a declared interval the agent does
/// not keep is the one way that field can lie.
const HEARTBEAT_MS: u64 = HEARTBEAT.as_millis() as u64;

/// Delay before the FIRST flush.
///
/// Shorter than the heartbeat so a launcher checking whether discovery works
/// does not have to wait half a minute for the first evidence. Long enough that
/// the counters mean something: PTP Sync runs at 1-8/s, so ten seconds is
/// hundreds of chances to have heard one.
const FIRST_FLUSH: Duration = Duration::from_secs(10);

/// Drop a clock that has gone quiet for this long.
///
/// PTP is continuous by nature: a healthy clock is never silent for 30s at any
/// standard rate. Silence therefore means gone, not idle.
const CLOCK_TIMEOUT: Duration = Duration::from_secs(30);


/// Blocking listen loop — run on a dedicated thread.
pub fn run_listen_agent(mqtt_host: &str, mqtt_port: u16) {
    let mut opts = rumqttc::MqttOptions::new("apkaudio-ptp", mqtt_host, mqtt_port);
    opts.set_keep_alive(Duration::from_secs(30));
    if let Some((username, password)) = apkaudio_contracts::broker_auth::credentials() {
        opts.set_credentials(username, password);
    }
    let (mqtt_client, mut connection) = rumqttc::Client::new(opts, 64);
    std::thread::spawn(move || {
        apkaudio_contracts::broker_backoff::drain(
            "ptp",
            connection.iter(),
            |e| matches!(e, rumqttc::ConnectionError::ConnectionRefused(_)),
            |_| {},
        );
    });

    let transports = Transports::open();
    for problem in &transports.problems {
        eprintln!("⚠️  [PTP] {problem}");
    }
    if !transports.any_open() {
        eprintln!("🛑 [PTP] no transport could be opened — agent not running");
        report(
            &mqtt_client,
            AgentReport::unavailable("ptp", &transports.problems.join(" | ")),
        );
        return;
    }

    // A partial capture is still worth running, but the bus must say which
    // half is missing — "no gPTP clocks" and "could not listen for gPTP" look
    // identical downstream otherwise.
    let detail = if transports.problems.is_empty() {
        format!(
            "{} UDP port(s), L2 {}",
            transports.udp.len(),
            if transports.l2.is_some() { "yes" } else { "no" }
        )
    } else {
        transports.problems.join(" | ")
    };
    let account = if transports.problems.is_empty() {
        AgentReport::listening("ptp", &detail)
    } else {
        AgentReport::partial("ptp", &detail)
    };
    // Which NIC, as a field rather than as prose. The detail line above says
    // how many transports opened; it cannot say WHERE, and "is the hunter up"
    // is really "is it up on the interface I care about".
    let account = account.map(|a| a.on(transports.interfaces.iter().map(|(name, ..)| name)));
    println!("🔎 [PTP] {}: {detail}", account.as_ref().map(|a| a.state().as_str()).unwrap_or("?"));
    report(&mqtt_client, account);

    // Started once per run: it owns a cache and one worker thread, and every
    // clock on a bench tends to come from a handful of manufacturers, so this
    // reaches steady state within a flush or two and never calls out again.
    let vendors = apkaudio_maclookup::MacVendors::start();

    let mut tracker = FlowTracker::default();
    let mut table = ClockTable::default();
    let mut diagnostics = Diagnostics::default();
    // The totals as of the last sample, so the growth can be published rather
    // than the total: `Sampler` counters are monotonic and take increments.
    let mut reported = Totals::default();
    let mut published: std::collections::HashMap<String, String> = Default::default();
    // The clocks as one document each, on the topic the NMOS bridge reads.
    // The twenty-two retained attributes stay where they are: they live under
    // `System/Protocols/…`, which `element.rs` treats as an agent root.
    let mut discovered = Register::new();
    // Start the clock in the past so the first flush lands at FIRST_FLUSH.
    let mut last_flush = Instant::now()
        .checked_sub(HEARTBEAT - FIRST_FLUSH)
        .unwrap_or_else(Instant::now);

    // Reception counters, split by transport, published as AgentCounters
    // (PLAN-34.02) rather than as two retained scalars.
    //
    // The split is diagnostic on its own: raw AF_PACKET capture sits BELOW
    // netfilter and UDP does not, so "L2 flowing, UDP zero" is the
    // unmistakable signature of a host firewall dropping inbound UDP — not of
    // a network without PTP on it.
    //
    // They are DECLARED at zero so the first sample carries both keys. A
    // counter that only appears once it is non-zero makes "no PTP on this
    // network yet" look identical to "this agent does not count that".
    let started_at_ms = unix_ms_now();
    let mut counters = Sampler::new("apkaudio-ptp", started_at_ms, HEARTBEAT_MS)
        .expect("apkaudio-ptp is a valid topic segment");
    counters.declare("udpSeen");
    counters.declare("l2Seen");
    // The synchronization faults, as counts rather than as levels. A lost Sync
    // is an event that happened once and stays true; the jitter it came with
    // is a level that is already different by the time anyone reads it, and
    // the two do not belong in the same payload.
    counters.declare("syncLost");
    counters.declare("syncReordered");
    counters.declare("reelections");
    counters.declare("grandmasterClassChanges");

    crate::monitor::run(
        &transports,
        &mut tracker,
        |obs| {
            match &obs {
                Observation::V2(_, _, meta) | Observation::V1(_, meta) => {
                    if meta.port.is_some() { counters.inc("udpSeen") } else { counters.inc("l2Seen") }
                }
                Observation::Undecodable { .. } => {}
            }
            match &obs {
                Observation::V2(msg, _, meta) => {
                    table.record(msg, meta);
                    diagnostics.observe(msg, meta);
                }
                // PTPv1 is recorded too, widened onto the same 64-bit identity
                // via the EUI-64 convention. Counting it but not recording it
                // (which this did) made a live v1 device invisible in the tab
                // while the agent cheerfully reported it was listening.
                Observation::V1(msg, meta) => {
                    table.record_v1(msg, meta);
                }
                Observation::Undecodable { .. } => {}
            }

            if last_flush.elapsed() >= HEARTBEAT {
                last_flush = Instant::now();
                // A grandmaster that fell silent is a state change no packet
                // announces. Without this sweep the dead master stays elected
                // and every clock under it keeps reporting itself healthy.
                diagnostics.settle(last_flush);
                flush(&mqtt_client, &mut table, &mut published, &mut discovered, &vendors, &diagnostics);
                diagnostics.retain_ports(|key| table.clocks.contains_key(key));
                count_faults(&mut counters, &mut reported, diagnostics.totals());
                report_reception(&mqtt_client, &counters, !transports.udp.is_empty());
            }
        },
        || false,
    );
}

/// Publish every known clock, and clear those that have gone quiet.
fn flush(
    mqtt_client: &rumqttc::Client,
    table: &mut ClockTable,
    published: &mut std::collections::HashMap<String, String>,
    discovered: &mut Register,
    vendors: &apkaudio_maclookup::MacVendors,
    diagnostics: &Diagnostics,
) {
    let now = Instant::now();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut live = std::collections::HashSet::new();

    for clock in table.clocks.values() {
        if now.saturating_duration_since(clock.last_seen) > CLOCK_TIMEOUT {
            continue;
        }
        let id_seg = sanitize_segment(&format_clock_id(&clock.key.clock_identity));
        let topic_seg = format!("{id_seg}-{}-d{}", clock.key.port_number, clock.domain);
        live.insert(topic_seg.clone());
        let prefix = format!("APK.audio/System/Protocols/ptp/Device/{topic_seg}");

        // The arrival stamp is remembered in `published` itself rather than in
        // a register beside it: that map is already the record of what this
        // clock was last told to be, and the stale sweep below already empties
        // it per topic — so a clock that goes away and comes back gets a NEW
        // spell for free, and there is no second structure to keep in step.
        let online_since = published
            .get(&format!("{prefix}/online_since"))
            .cloned()
            .unwrap_or_else(|| now_secs.to_string());

        let quality = ClockQuality {
            class: clock.grandmaster_class.unwrap_or(0),
            accuracy: clock.grandmaster_accuracy.unwrap_or(0xFE),
            offset_scaled_log_variance: 0,
        };
        let dash = || "-".to_string();

        let clock_id = format_clock_id(&clock.key.clock_identity);
        let values = [
            clock_id.clone(),
            // Non-blocking: unknown the first time a manufacturer appears, and
            // named from the next heartbeat on. This runs on the capture
            // thread, where a blocking HTTP call would drop Sync messages.
            vendors.vendor(&clock_id).unwrap_or_else(dash),
            clock.key.port_number.to_string(),
            clock.variant.label().to_string(),
            clock.domain.to_string(),
            if clock.is_grandmaster() { "grandmaster" } else { "clock" }.to_string(),
            clock.grandmaster.map(|g| format_clock_id(&g)).unwrap_or_else(dash),
            clock.grandmaster_class.map(|c| c.to_string()).unwrap_or_else(dash),
            clock.grandmaster_class.map(|_| quality.class_meaning().to_string()).unwrap_or_else(dash),
            clock.grandmaster_accuracy.map(|_| quality.accuracy_meaning().to_string()).unwrap_or_else(dash),
            clock.time_source.map(|t| time_source_meaning(t).to_string()).unwrap_or_else(dash),
            clock.steps_removed.map(|s| s.to_string()).unwrap_or_else(dash),
            clock.priority1.map(|p| p.to_string()).unwrap_or_else(dash),
            clock.priority2.map(|p| p.to_string()).unwrap_or_else(dash),
            clock.sync_interval.map(|i| format!("{i:.4}")).unwrap_or_else(dash),
            clock.two_step.to_string(),
            clock.subdomain.clone().unwrap_or_else(dash),
            clock.message_summary(),
            diagnostics.verdict(&clock.key, clock.grandmaster).label().to_string(),
            "identified".to_string(),
            presence::ONLINE_NOW.to_string(),
            online_since,
        ];

        // Only write what changed. A clock's attributes are static for hours at
        // a time; rewriting them every heartbeat is pure bus noise.
        //
        // `last_online` used to be exempt from that rule, because it carried
        // `now` and the Discovered table read a row's colour off the stamp's
        // age. Both presence fields are constants now — `-1` and the arrival
        // stamp — so a clock that has not changed writes nothing at all, and
        // the exception is gone with the thing that needed it.
        // The same clock as a DeviceRecord.
        //
        // IDENTITY IS THE CLOCK IDENTITY plus the port and domain — the same
        // `topic_seg` the retained tree is keyed on. The clock identity alone
        // is not enough: a boundary clock presents several ports, and two
        // domains on one grandmaster are two different time references that a
        // controller must be able to tell apart. Collapsing them would put one
        // row where the bench has three.
        //
        // "Clock" is the class because that is the word `element.rs::KEYWORDS`
        // maps to a source — which a grandmaster is, and which an ordinary
        // clock listening for Sync is not. That distinction is the direction
        // question, and it belongs to the bridge; what is stated here is what
        // the thing IS.
        let sighting = Sighting::new("ptp", &topic_seg)
            .class("Clock")
            .made_by(
                &vendors.vendor(&clock_id).unwrap_or_else(dash),
                if clock.is_grandmaster() { "PTP Grandmaster" } else { "PTP Clock" },
            )
            .serial(&topic_seg)
            .raw_idn(&clock_id)
            .notes(&format!(
                "domain {}, port {}, {} steps removed",
                clock.domain,
                clock.key.port_number,
                clock.steps_removed.map(|s| s.to_string()).unwrap_or_else(dash)
            ))
            .status(DeviceRecordStatus::Identified);
        match discovered.observe(&sighting, now_secs as i64 * 1_000) {
            Ok(Some(publication)) => {
                let _ = mqtt_client.publish(
                    publication.topic,
                    rumqttc::QoS::AtLeastOnce,
                    true,
                    publication.payload.into_bytes(),
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("   ⚠️  [PTP] {clock_id} not published as a DeviceRecord: {e}"),
        }

        for (key, value) in CLOCK_KEYS.iter().zip(values) {
            let topic = format!("{prefix}/{key}");
            if published.get(&topic).is_some_and(|p| *p == value) {
                continue;
            }
            let _ = mqtt_client.publish(
                topic.clone(),
                rumqttc::QoS::AtLeastOnce,
                true,
                value.clone().into_bytes(),
            );
            published.insert(topic, value);
        }
    }

    // Clear clocks that stopped talking.
    let stale: Vec<String> = published
        .keys()
        .filter_map(|t| {
            let seg = t.strip_prefix("APK.audio/System/Protocols/ptp/Device/")?;
            let seg = seg.split('/').next()?;
            (!live.contains(seg)).then(|| seg.to_string())
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for seg in stale {
        println!("   ⌛ [PTP] {seg} stopped announcing — clearing");
        // The DeviceRecord too. `topic_seg` IS the serial, so the id follows
        // from the stale segment; the register remembers the rest.
        if let Some(publication) = discovered.retract_id(&format!("ptp:{seg}")) {
            let _ = mqtt_client.publish(
                publication.topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                Vec::new(),
            );
        }
        let prefix = format!("APK.audio/System/Protocols/ptp/Device/{seg}");
        for key in CLOCK_KEYS {
            let topic = format!("{prefix}/{key}");
            let _ = mqtt_client.publish(topic.clone(), rumqttc::QoS::AtLeastOnce, true, Vec::new());
            published.remove(&topic);
        }
    }

    table.clocks.retain(|_, c| now.saturating_duration_since(c.last_seen) <= CLOCK_TIMEOUT);
}

/// Turn the diagnostics' running totals into counter increments.
///
/// `Sampler` counts monotonically and offers no way to decrease a counter,
/// which is the contract working: a fault that happened does not un-happen.
/// But `retain_ports` takes a departed clock's share of the totals away with
/// it, so the subtraction can go negative — and a saturating one is the only
/// honest reading of that. It publishes no growth for the flush in which a
/// clock left, rather than a negative rate or a wrapped enormous one.
fn count_faults(counters: &mut Sampler, reported: &mut Totals, now: Totals) {
    counters.add("syncLost", now.sync_lost.saturating_sub(reported.sync_lost));
    counters.add("syncReordered", now.sync_reordered.saturating_sub(reported.sync_reordered));
    counters.add("reelections", now.reelections.saturating_sub(reported.reelections));
    counters.add(
        "grandmasterClassChanges",
        now.class_changes.saturating_sub(reported.class_changes),
    );
    *reported = now;
}

/// Publish what has actually been received, per transport.
///
/// TWO TOPICS, AND THEY ARE DELIBERATELY SHAPED DIFFERENTLY
///
/// The counts go out as `AgentCounters` on `System/Counters/apkaudio-ptp`,
/// NOT RETAINED: a count is a sample, and a retained sample keeps answering
/// after this process is dead. `startedAt` and `intervalMs` travel with it so
/// a consumer can difference two samples into a rate and can tell a restart
/// from a lost message. Before PLAN-34.02 these were two retained scalars
/// under `Agent/`, which no consumer read and which outlived the agent.
///
/// `udp_blocked` stays a RETAINED scalar because it is not a count — it is a
/// conclusion about this host's firewall that remains true while the agent is
/// down, and four consumers already read it there (`launch.py`,
/// `read-discovered-clocks.js`, the orchestrator's agent map, and
/// `interfaces_reporter.py`). Publishing the conclusion rather than the raw
/// numbers means none of them re-derives the netfilter reasoning.
fn report_reception(
    mqtt_client: &rumqttc::Client,
    counters: &Sampler,
    udp_open: bool,
) {
    match counters.publication(unix_ms_now()) {
        Ok((topic, payload, retain)) => {
            let _ = mqtt_client.publish(topic, rumqttc::QoS::AtLeastOnce, retain, payload);
        }
        // A malformed sample is a bug in this agent, not a reason to take the
        // capture loop down: say so once per flush and keep listening.
        Err(e) => eprintln!("⚠️  [PTP] counters not published: {e}"),
    }

    let udp = counters.get("udpSeen");
    let l2 = counters.get("l2Seen");
    let blocked = udp_open && udp == 0 && l2 > 0;
    let _ = mqtt_client.publish(
        "APK.audio/System/Protocols/ptp/Agent/udp_blocked",
        rumqttc::QoS::AtLeastOnce,
        true,
        blocked.to_string().into_bytes(),
    );
}

/// Wall-clock now, in Unix milliseconds. `AgentCounters` timestamps are the
/// x-axis every rate is divided by, so they must be wall clock and not the
/// `Instant` the flush cadence uses.
fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Report whether this agent can see the network.
///
/// Same reasoning as the AVB agent: the orchestrator infers `online` from the
/// presence of a `config.ini`, which cannot know that a capability is missing.
///
/// The topics, the words and the retain flag are all `agent_state`'s now. This
/// crate and `apkaudio-AVB-Milan` each held a private copy of them, which is a
/// convention rather than a contract — and a convention is what the third
/// publisher disagrees with.
fn report(mqtt_client: &rumqttc::Client, account: Result<AgentReport, TopicError>) {
    let publications = account.and_then(|a| a.publications());
    match publications {
        Ok(entries) => {
            for (topic, payload, retain) in entries {
                let _ = mqtt_client.publish(topic, rumqttc::QoS::AtLeastOnce, retain, payload);
            }
        }
        // A protocol name this agent controls cannot fail to be a segment, so
        // this is a bug here rather than a bus condition: say so and keep
        // capturing.
        Err(e) => eprintln!("⚠️  [PTP] agent state not published: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_segments_keep_clock_identities_readable() {
        // Clock identities are colon-separated on the wire; topics use "_".
        assert_eq!(sanitize_segment("00:0A:92:FF:FE:01:56:A3"), "00_0A_92_FF_FE_01_56_A3");
        assert_eq!(sanitize_segment("a/b+c#d"), "a_b_c_d");
    }

    /// One device can run several ports and domains; the topic segment has to
    /// separate them or they overwrite each other.
    #[test]
    fn segment_distinguishes_port_and_domain() {
        let id = sanitize_segment("00:0A:92:FF:FE:01:56:A3");
        let a = format!("{id}-{}-d{}", 1, 0);
        let b = format!("{id}-{}-d{}", 1, 127);
        let c = format!("{id}-{}-d{}", 2, 0);
        assert_ne!(a, b, "domains must not collide");
        assert_ne!(a, c, "ports must not collide");
    }

    #[test]
    fn key_count_is_stable() {
        assert_eq!(CLOCK_KEYS.len(), 22);
    }

    /// A departing clock lowers a total that only ever rises. The subtraction
    /// must floor at zero rather than wrap to eighteen quintillion.
    #[test]
    fn a_total_that_fell_publishes_no_growth() {
        let mut counters = Sampler::new("apkaudio-ptp", 0, 1000).unwrap();
        counters.declare("syncLost");
        counters.declare("syncReordered");
        counters.declare("reelections");
        counters.declare("grandmasterClassChanges");
        let mut reported = Totals::default();

        count_faults(&mut counters, &mut reported, Totals { sync_lost: 9, ..Totals::default() });
        assert_eq!(counters.get("syncLost"), 9);

        // The clock carrying those nine went away.
        count_faults(&mut counters, &mut reported, Totals::default());
        assert_eq!(counters.get("syncLost"), 9, "the count holds, it does not wrap");

        // ...and the next fault after that still lands.
        count_faults(&mut counters, &mut reported, Totals { sync_lost: 2, ..Totals::default() });
        assert_eq!(counters.get("syncLost"), 11);
    }
}
