# PLUGIN.PTP delivery

← [Publishing](INDEX.md) · [Repo README](../../README.md)

## Built and tested

The crate `apkaudio-ptp` (`SRC/`) is a member of the plugin workspace
`PODS/POD.THICK/DISCOVERY.Engine/SRC`: `cargo test -p apkaudio-ptp` there. The binary `apk-ptp` is
compiled with every other plugin into the shared image `apk-plugins:local` by
[`DISCOVERY.Engine/Docker/Dockerfile`](../../../../DISCOVERY.Engine/Docker/Dockerfile) (target
`plugins`; the build context is the APK.audio repo root). This repo has no Dockerfile.

## Shipped

As the container `Plugin-PTP` from [`Docker/docker-compose.yml`](../../Docker/docker-compose.yml):
service `ptp`, profile `l2`, host network, `cap_add: [NET_RAW, NET_BIND_SERVICE]` (the Dockerfile also sets them as file capabilities on `/usr/local/bin/apk-ptp`), `extends: plugin-host` in
[`plugin-base.yml`](../../../../DISCOVERY.Engine/Docker/plugin-base.yml) (image, `cap_drop: [ALL]`,
read-only root, broker variables, log and OUI-cache volumes, the runner's healthcheck).
`DISCOVERY.Engine/Docker/docker-compose.yml` `include:`s this file into the compose project
`apk-audio`, so it starts with the other plugins on a node whose role list names `l2`.

It lands on the node's broker, under `APK.audio/System/Protocols/ptp/…` (unretained, restated every 30 s), `APK.audio/System/Counters/apkaudio-ptp` and `APK.audio/Discovery/ptp/…` (presence, retained); its `Agent/udp_blocked` verdict is read by the launcher and the OS Clock page. There is no live URL. The repository
(https://github.com/LikeDotAudio/APK.plugin.clock.PTP.git) is pushed with every other submodule by `.apk.scripts/deploy_all.py` at the APK.audio
root.

## Not automated

* CI is the SDK's reusable `plugin-ci.yml@sdk-v0.5` on every push; the monorepo CI builds nothing here, on purpose.
* No release artefact or tag: the image is built locally on each node (`up -d --build`); tags are the owner's.
* `ptp-monitor` is built into the image (`/usr/local/bin`) but has no file capabilities there; run by hand it needs `sudo` or `setcap cap_net_raw,cap_net_bind_service+eip`.

## Delivered from the 2026-10-30 audit

PLUGIN.PTP is one plugin (`Plugin-PTP`) in the shared plugin image. Verified with
`cargo test -p apkaudio-ptp` in the DISCOVERY.Engine workspace (85 passed incl. the rustfmt test;
the PTPv2 parser's 10 tests now run in PLUGIN.RAVENNA, with 2 more for AES67's needs) and
`COMPOSE_PROFILES=l2 docker compose -f Docker/docker-compose.yml config -q`.

- **Shared say / restate / report and session** (#1, finding 4) — the local `say`, `report`, `restate`, `publications_of` and the `MqttOptions` / `broker_backoff::drain` block are deleted; `src/agent.rs` uses `apk_plugin_runner::unkept::{say, restate, report, publications_of}` and `Session::new("ptp", "listen")…spawn_drained()`. The unkept source test now forbids any direct publish. (Standalone needs the next SDK tag — owner.)
- **Topic root in one place** (#2, finding 3) — already `device_prefix` over the contract's `topics::protocols` and `agent_state::agent_topic` (commit 898b6e8, test `device_and_agent_topics_are_the_contracts`); no `APK.audio/…` literal is left outside tests.
- **One PTP parser for the monorepo** (the AES67 duplication item) — `src/message.rs` (685 lines) moved to `apkaudio_ravenna::ptp` (PLUGIN.RAVENNA) with its tests, extended with what AES67's slave needs (`encode_delay_req`, `Timestamp::as_nanos`, `correction_whole_ns`, port keys, RFC 7273 clock-id text); `message.rs` is now its re-export, so `crate::message::…` and `ptp-monitor` are unchanged. `apkaudio-ravenna` taken by `rev` with `default-features = false`.
- **CI pinned** (#3, part) — `.github/workflows/ci.yml` → `plugin-ci.yml@sdk-v0.5`. Tagging this crate's releases: Needs owner (still in the audit).
- **Python binding dropped** (#5, finding 6) — the pyo3 `clock` / `parser` shims had no consumer anywhere in the monorepo (grep for `oaptpparser`, `oa_ptp`, `--features python`) and carried a second private PTP parser; `src/clock.rs`, `src/parser.rs`, the `python` feature and the `pyo3` / `parking_lot` optional deps are removed (history keeps them). README and ARCHITECTURE updated.
- **Payload-shape test against the contracts' DeviceRecord** (#6) — the clock sighting is now `clock_sighting()`; `the_presence_document_is_a_contract_device_record` round-trips its record through the generated `DeviceRecord` type and, in the monorepo, checks it against `contracts/schemas/DeviceRecord.schema.json` (every required key, no undeclared one; identity = the clock segment). The `Device/{clock}` leaves are not DeviceRecords; `key_count_is_stable` and the CLOCK_KEYS order test cover them.
- **Shared plugin volumes** (finding 5) — the copied logs-volume block is `include: ../../../DISCOVERY.Engine/Docker/plugin-volumes.yml`.
- **Kept by design: compose `extends` plugin-base.yml and the engine `[patch]` / `members/` link** (#4, finding 5) — the house plugin pattern; reason in PRINCIPLES.md.
