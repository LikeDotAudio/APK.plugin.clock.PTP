// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! PTP (IEEE 1588) clock, parser and monitor.
//!
//! ONE lint is allowed crate-wide and it is not this crate's code:
//! `unsafe_op_in_unsafe_fn` fires inside pyo3's `#[pymethods]` expansion in
//! `clock` and `parser`. The four-lint blanket this replaced hid an unbound
//! spec constant across a crate carrying 44 tests.
#![allow(unsafe_op_in_unsafe_fn)]
//! `apkaudio-ptp` — PTP (IEEE 1588) discovery and live traffic monitoring.
//!
//! Watches all three clock protocols that can share one NIC — PTPv1
//! (1588-2002), PTPv2 (1588-2008) and gPTP (802.1AS) — decodes their messages,
//! and correlates them into the exchanges they actually form (Sync ↔ Follow_Up,
//! Delay_Req ↔ Delay_Resp, the three-legged peer-delay chain).
//!
//! * [`message`] — PTPv2 / gPTP parsing
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
//! # Legacy PyO3 shim
//!
//! The `clock` and `parser` modules predate this and remain gated behind the
//! non-default `python` feature. They were `oa_ptp_clock_rs` and
//! `oa_ptp_parser_rs` until PLAN-130.16; the Rust implementation above does
//! not depend on either.
pub mod diagnostics;
pub mod flow;
pub mod message;
pub mod monitor;
pub mod net;
pub mod report;
pub mod v1;

mod agent;
pub use agent::run_listen_agent;

#[cfg(feature = "python")]
pub mod clock;

#[cfg(feature = "python")]
pub mod parser;

/// This crate's own self test — what it can prove about itself with nothing
/// plugged in. Reached from the bus through `outgoing/selftest_run`; see
/// `apkaudio_contracts::self_test` for the two inputs and the withdrawal the
/// first of them performs. PLAN-459.01.
pub mod self_test;
