# APK:plugin:PTP

PTP v1 + v2 + gPTP listener, one container — and `ptp-monitor`, the hand tool beside it.

**Wiki:** [Documentation/README.md](Documentation/README.md) — principles, architecture and delivery, each checked against the code.

| | |
|---|---|
| **Recipe** | `Docker/docker-compose.yml` — no Dockerfile: it runs a binary from the shared plugin image built by `PODS/POD.THICK/DISCOVERY.Engine/Docker/Dockerfile`, extending `plugin-host` in `DISCOVERY.Engine/Docker/plugin-base.yml` |
| **Source** | `SRC/` (this repository — https://github.com/LikeDotAudio/APK.plugin.clock.PTP.git) — crate `apkaudio-ptp`, a member of the `DISCOVERY.Engine/SRC` workspace; bins `apk-ptp` (the agent, `apk_plugin_runner::main!(token = "ptp", run = [run_listen_agent])`) and `ptp-monitor` (the hand tool) |
| **Container** | `Plugin-PTP`, command `apk-ptp` |
| **Profile** | `l2` |
| **Network** | host |
| **Capability** | `NET_RAW` (gPTP over raw Ethernet, AF_PACKET, EtherType 0x88F7) and `NET_BIND_SERVICE` (UDP 319/320) — `cap_add` in compose plus the file capabilities in the Dockerfile. Without the second the agent reports `partial`: gPTP is still heard, UDP clocks are not |
| **Talks to** | the broker only — it listens to PTP and sends none |
| **Bus token** | `ptp` |
| **Publishing** | **nothing is kept** — every topic at QoS 0, retain clear; the whole picture restated every 30s |

Was a thread in `workers/discovery.rs`. Moved into its own container by PLAN-3316.01.

## What it does

Listens on UDP 319/320 and raw Ethernet at once (`src/net.rs`), decodes PTPv1
(`src/v1.rs`), PTPv2 and gPTP (`src/message.rs` — the shared `apkaudio_ravenna::ptp` reader), pairs messages into the
exchanges they form — Sync ↔ Follow_Up, Delay_Req ↔ Delay_Resp, the peer-delay
chain (`src/flow.rs`) — and reads three diagnostics: offset from master,
arrival jitter and BMCA election (`src/diagnostics/`). The clock table
(`src/monitor.rs`) is flushed to the bus 10 s after start (`first_flush_s`) and then every 30 s
(`heartbeat_s`, `src/agent.rs`); a clock not heard for 30 s (`clock_timeout_s`) is dropped and
its presence goes inactive.

```
APK.audio/System/Protocols/ptp/Device/{clock_id}-{port}-d{domain}/{key}   ← QoS 0, retain clear
  key ∈ clock_id vendor port variant domain role grandmaster gm_class gm_class_meaning
        gm_accuracy time_source steps_removed priority1 priority2 sync_interval_s
        two_step subdomain messages sync_health status last_online online_since
APK.audio/System/Protocols/ptp/Agent/udp_blocked                          ← QoS 0, retain clear
APK.audio/System/Counters/apkaudio-ptp                                    ← AgentCounters, QoS 0
```

`sync_health` is a verdict word (`jittery`, …), not the numbers; the numbers
are in `ptp-monitor --json`. `vendor` is the OUI inside the clock identity,
through `LIB.MAC_LOOKUP`.

## Nothing this agent publishes is kept

Every publish goes out at **QoS 0 with the retain flag clear** — the one
combination MQTT offers that the broker neither stores on the topic, nor queues
for an absent subscriber, nor writes to its persistence file. A PTP fact is true
only while a clock is on the wire saying so, and a retained copy keeps answering
long after the clock is unplugged — an answer nothing can correct, because the
agent that would correct it is gone too.

The price is paid deliberately: **everything is republished every 30s heartbeat,
not just what changed.** Publishing only the delta is correct while the broker
holds the last value and wrong when it does not — a clock whose attributes are
static for hours (the normal case) would be announced once and then be invisible
to every subscriber that connected afterwards.

What this means for readers:

| | |
|---|---|
| A fresh subscriber | has the full picture within one heartbeat (≤30s) |
| A clock that goes away | stops being restated; nothing is left to clear |
| While the agent is down | the PTP tree is **empty**, including `Agent/udp_blocked` |

`Agent/udp_blocked` used to be retained precisely so it stayed readable while
this agent was down. It is restated every heartbeat instead, so its readers
(`WEB.Frontend-Assets/SRC/launcher/launch.py` and `launch_host_tools.py`,
`OS/Clock/discovered-clocks/read-discovered-clocks.js`,
`NODE.BareMetal/SRC/Core/ORCHESTRATOR:hunter:VISA/src/discovered/`,
`Baremetal:Manager/interfaces_report.py`) see it within a heartbeat and see nothing once the
agent stops — the honest reading of "this host's firewall state is unknown".

Two things stay retained, because neither belongs to this crate: the presence
document at `Discovery/ptp/{id}` and the `config.ini` publication, both owned by
`apk-plugin-runner` and shared with every plugin. Presence is what says a clock
has *gone* rather than merely stopped being restated.

`agent.rs::nothing_this_agent_publishes_is_kept` enforces this at the source: no
direct publish call in the agent; every one is the runner's `unkept::say` /
`restate` / `report`.

## `ptp-monitor`

The live, packet-by-packet view of the same capture — a tool you watch, not a
state store (`src/bin/ptp-monitor.rs`). It needs the same two capabilities
(`sudo`, or `setcap cap_net_raw,cap_net_bind_service+eip ptp-monitor`).

```sh
ptp-monitor                     # live tail, each message linked to its partner
ptp-monitor --summary           # clock table, refreshed
ptp-monitor --seconds 30        # stop after 30 s and print the summary
ptp-monitor --domain 0          # one domain only
ptp-monitor --json --seconds 6  # one JSON object on stdout (report::snapshot_json), human lines on stderr
```

## Configuration

`SRC/config.ini`, `[ptp]`: `enabled`, `topic = APK.audio/System/Protocols/ptp`
(read by the runner as the module topic), `domain = 0` (read by nothing), and
the presence pair — `presence_protocols = ptp`, `presence_inactive_after_s = 90`
(3 × the flush). The broker is `MQTT_HOST` / `MQTT_PORT` from the runner, which
publishes the retained config at `APK.audio/System/Protocols/ptp/config` and
runs the self test (`src/self_test.rs`). `SRC/gui/manifest.json` and
`SRC/gui/ptp/ptp.json` are the clock console panel. Each clock presents its own panel (PLAN-3413.01): `apk-ptp.rs` names `panels = [ptp]`, so apk-plugin-runner binds the template to every clock under `…/ptp/Device/<clock>` (`device_depth: 1`) and keeps it retained at `APK.audio/System/Gui/Panels/ptp-<clock>`, emptied when `last_online` says offline or after `panels_expire_after_s` (120 s) of silence. The "Online since" field reads `online_since`. There is no Python binding:
the legacy pyo3 `clock`/`parser` shims had no consumer and were dropped (2026-10-30 audit).

## Build, run, test

```sh
# from PODS/POD.THICK/DISCOVERY.Engine/SRC
cargo test -p apkaudio-ptp                       # 85 tests, one is `cargo fmt --check` (2026-10-30; the PTPv2 parser's tests live with it in PLUGIN.RAVENNA)
cargo build -p apkaudio-ptp --bin ptp-monitor

COMPOSE_PROFILES=l2 docker compose -f PODS/POD.THICK/PLUGINS/PLUGIN.PTP/Docker/docker-compose.yml up -d --build
```

`apk-ptp` takes no arguments — `--help` starts the agent against the broker.

## Philosophy

- **A clock fact is true only while the clock says it.** Nothing kept, the
  whole picture restated, presence the one retained witness of departure
  (`src/agent.rs` header, `unkept::say`, `nothing_this_agent_publishes_is_kept`).
- **Unreachable is dead.** A clock silent for 30 s leaves the table and its
  presence goes inactive (`clock_timeout_s` in `config.ini`, `src/agent.rs`).
- **A verdict, not a firehose.** `sync_health` is one word that changes when the
  health does; the numbers are a document you ask `ptp-monitor --json` for
  (`CLOCK_KEYS`).
- **Say the partial truth as partial.** Missing a transport is `partial` with
  the reason, not silence and not a crash (`src/net.rs`, `AgentReport::partial`
  in `src/agent.rs`).
- **Conclusions travel, not raw splits.** `udp_blocked` is the firewall verdict
  (L2 flowing, UDP zero) so readers do not re-derive it (`report_reception`).
- **Identity is never decorated.** `vendor` is its own key so `clock_id` stays
  the topic segment and the grandmaster match (`CLOCK_KEYS`).
- **Least privilege, named.** Two capabilities, each for a stated transport
  (compose, Dockerfile).

---

*© 2026 — Written & Maintained by Anthony P. Kuzub. MIT Licence.*

<!-- wiki:smoke -->
**Smoke test** — ✅ PASS 2/2 since 2026-10-30 12:51 EDT — [results](Documentation/%F0%9F%A7%A9Manual%20and%20Artifacts/SMOKE.md)
<!-- /wiki:smoke -->
