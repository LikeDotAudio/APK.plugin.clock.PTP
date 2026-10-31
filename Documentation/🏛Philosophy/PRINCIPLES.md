# Principles PLUGIN.PTP keeps

Each rule is pointed at the code that keeps it; where the code bends one, it says so. Ecosystem
rules live in the root: [Refusal over fabrication](../../../../../../Documentation/%F0%9F%8F%9BPhilosophy/Doctrine/readme.Refusal%20over%20fabrication.md),
[Doctrine](../../../../../../Documentation/%F0%9F%8F%9BPhilosophy/Doctrine/readme.Doctrine.md).

← [Philosophy](INDEX.md) · [Repo README](../../README.md)

| Principle | Kept by |
|---|---|
| **A clock fact is true only while the clock says it.** Every publish is QoS 0, retain clear, through one helper; the whole picture is restated every 30 s, so a late subscriber sees it and a stopped agent leaves the tree empty. | `say`, [`SRC/src/agent.rs:159`](../../SRC/src/agent.rs); `heartbeat_s` in `config.ini`, read by `Settings::from_ini` (`:104`); test `nothing_this_agent_publishes_is_kept`. |
| **Unreachable is dead.** A clock not heard for 30 s leaves the table and its presence goes inactive. | `clock_timeout_s` in `config.ini` (`Settings`, `SRC/src/agent.rs:104`); `presence::gone`, `:518`. |
| **Identity is the clock, never an address.** The device key is the clock identity (EUI-64, which carries the MAC) plus port and domain; it is the sighting's serial. `vendor` is its own key so `clock_id` stays the identity. | `.serial(&topic_seg)`, `SRC/src/agent.rs:357`; `CLOCK_KEYS` (`:56`). |
| **Say the partial truth as partial.** A transport that cannot be opened (no `NET_BIND_SERVICE`, no `NET_RAW`) makes the agent report `partial` with the reason, and it keeps listening on what it has. | `AgentReport::partial`, `SRC/src/agent.rs:166`; [`SRC/src/net.rs`](../../SRC/src/net.rs) header. |
| **Conclusions travel, not raw splits.** `udp_blocked` is the firewall verdict (L2 flowing, UDP silent), so readers do not re-derive it; `sync_health` is one word, the numbers are `ptp-monitor --json`. | `report_reception`, `SRC/src/agent.rs:574`; `Diagnostics::verdict`, [`SRC/src/diagnostics.rs:172`](../../SRC/src/diagnostics.rs). |
| **Listen, never send.** No PTP message is transmitted; it cannot disturb a clock election. | [`SRC/src/net.rs`](../../SRC/src/net.rs) (receive sockets only), README "Talks to". |
| **Least privilege, named.** Two capabilities, each for a stated transport; everything else is dropped by the base service. | [`Docker/docker-compose.yml`](../../Docker/docker-compose.yml) `cap_add`; `plugin-base.yml` `cap_drop: [ALL]`. |

## Where the code bends a rule

* **Stall watchdog.** The capture loop calls `apk_plugin_runner::tick()` on every pass (at least every
  200 ms, the sockets' read timeout), so a loop that wedges without returning is an exit after
  `APK_PLUGIN_STALL_SECS`.
* **Declared config is read config.** `domain = 0` in [`SRC/config.ini`](../../SRC/config.ini) is read
  by nothing; the agent hears every domain.
