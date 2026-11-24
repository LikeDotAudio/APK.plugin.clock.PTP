// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! PTP (IEEE 1588) parser and monitor.
//! `apkaudio-ptp` — PTP (IEEE 1588) discovery and live traffic monitoring.
//!
//! Watches all three clock protocols that can share one NIC — PTPv1
//! (1588-2002), PTPv2 (1588-2008) and gPTP (802.1AS) — decodes their messages,
//! and correlates them into the exchanges they actually form (Sync ↔ Follow_Up,
//! Delay_Req ↔ Delay_Resp, the three-legged peer-delay chain).
//!
//! * [`message`] — PTPv2 / gPTP parsing (the shared `apkaudio_ravenna::ptp`)
//! * [`v1`] — PTPv1, which shares the wire but not the header layout
//! * [`net`] — capture across UDP (319/320) and raw Ethernet (0x88F7)
//! * [`flow`] — correlating messages into conversations
//! * [`diagnostics`] — offset from master, arrival jitter and BMCA election
//!   state: the three readings that separate a network that carries audio
//!   from one that drops it
//! * [`monitor`] — the shared capture loop and clock table
//! * [`report`] — a finished capture as one JSON object, for the machine
//!   reporter's Interfaces → Clock tab
//!
//! The `ptp-monitor` binary is the live view; [`run_listen_agent`] is the MQTT
//! agent. Both consume the same stream.
//!
//! There is no Python binding: the legacy pyo3 shims (`clock`, `parser`,
//! feature `python`) had no consumer and were dropped (2026-10-30 audit).
pub mod diagnostics;
pub mod flow;
pub mod message;
pub mod monitor;
pub mod net;
pub mod report;
pub mod v1;

#[cfg(feature = "agent")]
mod agent;
#[cfg(feature = "agent")]
pub use agent::run_listen_agent;

/// This crate's own self test — what it can prove about itself with nothing
/// plugged in. Reached from the bus through `outgoing/selftest_run`; see
/// `apkaudio_contracts::self_test` for the two inputs and the withdrawal the
/// first of them performs. PLAN-459.01.
#[cfg(feature = "agent")]
pub mod self_test;
