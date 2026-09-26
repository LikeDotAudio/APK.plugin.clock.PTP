# APK:plugin:PTP

PTP v1 + v2 + gPTP listener, one container — one of the four holding NET_RAW (with MILAN, PING and the discovery engine).

| | |
|---|---|
| **Recipe** | `Docker/docker-compose.yml` — no Dockerfile: it runs a binary from the shared plugin image built by `APK:discovery` |
| **Source** | `SRC/` (this repository — https://github.com/LikeDotAudio/APK.plugin.clock.PTP.git) — bin `apk-ptp`; `ptp-monitor` is the hand tool beside it; built by the shared plugin image |
| **Profile** | `l2` |
| **Talks to** | the broker only |
| **Publishing** | **nothing is kept** — every topic at QoS 0, retain clear; the whole picture restated every 30s |

Was a thread in `workers/discovery.rs`. Moved into its own container by PLAN-3316.01.

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
this agent was down. It is restated every heartbeat instead, so its four readers
(`launch.py`, `read-discovered-clocks.js`, the orchestrator's agent map,
`interfaces_reporter.py`) see it within a heartbeat and see nothing once the
agent stops — the honest reading of "this host's firewall state is unknown".

Two things stay retained, because neither belongs to this crate: the presence
document at `Discovery/ptp/{id}` and the `config.ini` publication, both owned by
`apk-plugin-runner` and shared with every plugin. Presence is what says a clock
has *gone* rather than merely stopped being restated.

`agent.rs::nothing_this_agent_publishes_is_kept` enforces this at the source: one
publish call in the crate, and it is `say`.

---

*© 2026 — Written & Maintained by Anthony P. Kuzub. MIT Licence.*
