// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//! `apk-ptp` — PTP v1 + v2 + gPTP listen as its own container (PLAN-3316.01).
//!
//! The other binary in the plugin image with file capabilities: NET_RAW for gPTP over raw
//! Ethernet and NET_BIND_SERVICE for UDP 319/320. `ptp-monitor` beside it is the hand tool.

//!
//! `panels = [ptp]`: every clock presents its own panel, retained at
//! `APK.audio/System/Gui/Panels/ptp-<clock>` (`device_depth: 1`).

apk_plugin_runner::main!(
    token = "ptp",
    lib = apkaudio_ptp,
    run = [run_listen_agent],
    panels = [ptp]
);
