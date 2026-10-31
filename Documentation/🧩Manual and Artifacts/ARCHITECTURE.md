# PLUGIN.PTP architecture

How to use it, and how it works inside. Every topic key and every `ptp-monitor` flag is in the
[repo README](../../README.md).

← [Manual and Artifacts](INDEX.md)

## Use

```sh
# from PODS/POD.THICK/DISCOVERY.Engine/SRC (the cargo workspace)
cargo test -p apkaudio-ptp          # tests/fmt.rs fails the suite when the crate is not rustfmt-clean
cargo build -p apkaudio-ptp --bin ptp-monitor
sudo target/debug/ptp-monitor --summary --seconds 30     # the live view, by hand
COMPOSE_PROFILES=l2 docker compose -f PODS/POD.THICK/PLUGINS/PLUGIN.PTP/Docker/docker-compose.yml up -d --build
```

## Two binaries, one library

| Binary | What |
|---|---|
| [`apk-ptp`](../../SRC/src/bin/apk-ptp.rs) | the agent: `apk_plugin_runner::main!(token = "ptp", run = [run_listen_agent])` |
| [`ptp-monitor`](../../SRC/src/bin/ptp-monitor.rs) | the hand tool: live tail, `--summary`, `--json` (`report::snapshot_json`) |

| Module | What |
|---|---|
| [`net.rs`](../../SRC/src/net.rs) | opens the transports: UDP 319/320 joined to the PTP groups, and AF_PACKET for EtherType 0x88F7 |
| [`message.rs`](../../SRC/src/message.rs), [`v1.rs`](../../SRC/src/v1.rs) | decode PTPv2/gPTP (re-exporting the shared `apkaudio_ravenna::ptp`, PLUGIN.RAVENNA) and PTPv1 into one `PtpMessage` |
| [`flow.rs`](../../SRC/src/flow.rs) | `FlowTracker` pairs Sync↔Follow_Up, Delay_Req↔Delay_Resp and the peer-delay chain |
| [`diagnostics/`](../../SRC/src/diagnostics.rs) | offset, arrival jitter, BMCA election → a `Verdict` per port |
| [`monitor.rs`](../../SRC/src/monitor.rs) | `ClockTable` and the receive loop shared by both binaries |
| [`report.rs`](../../SRC/src/report.rs) | counters and the JSON snapshot |
| [`agent.rs`](../../SRC/src/agent.rs) | `run_listen_agent`: flush, presence, counters, `udp_blocked` |

## Data flow

1. `net.rs` opens every transport it can; what it cannot open becomes the `partial` detail.
2. `monitor::run_paced` reads frames (the agent; `ptp-monitor` uses `monitor::run`), `classify` decodes them, `FlowTracker` links each to its partner and
   `Diagnostics` folds them into per-port verdicts; `ClockTable` keeps the last word from each clock.
3. `agent::flush` runs `first_flush_s` (10 s) after start and every `heartbeat_s` (30 s) — both `config.ini`, read by `agent::Settings::from_ini` — timed off the capture loop's per-pass pulse
   (every ≤ 200 ms), not off packets, so it still runs when all PTP traffic stops: drop clocks silent for `clock_timeout_s` (30 s) (`gone` to
   presence), then publish every live clock's 22 `CLOCK_KEYS` at
   `APK.audio/System/Protocols/ptp/Device/{clock_id}-{port}-d{domain}/{key}`, `seen` to presence,
   the counters to `APK.audio/System/Counters/apkaudio-ptp` and the `Agent/udp_blocked` verdict.

The runner owns the retained `config`, `incoming/state`, `incoming/api`, the self test
([`self_test.rs`](../../SRC/src/self_test.rs)) and the presence tracker.
