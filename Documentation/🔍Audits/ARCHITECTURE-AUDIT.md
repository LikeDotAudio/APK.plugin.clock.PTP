# 🔍 Architecture audit — PLUGIN.PTP (2026-10-30)

Audited against the APK.audio multi-repo architecture standard: coupling, boundaries, duplication, contracts.
← [Audits](INDEX.md) · Improvements: [🛣Roadmap/IMPROVEMENTS.md](../%F0%9F%9B%A3Roadmap/IMPROVEMENTS.md)

## Bounded context
PTP v1/v2 (UDP 319/320) and gPTP (raw Ethernet 0x88F7) listener: clocks, grandmaster election, offsets and arrival diagnostics, published under `APK.audio/System/Protocols/ptp`. Rust 2024, ~6.7 k lines, two bins (`apk-ptp`, `ptp-monitor`) and an optional pyo3 Python binding (all files read).

## Context map
| Talks to | How (topic / endpoint / file / import) | Relationship |
|---|---|---|
| APK.plugin.SDK (`apk-plugin-runner`, `apkaudio-contracts`) | Cargo git dep, `tag = "sdk-v0.5"` ([SRC/Cargo.toml](../../SRC/Cargo.toml)) | Conformist to the SDK (Shared Kernel for topic grammar, self-test, presence, broker auth/backoff) |
| DISCOVERY.Engine | compose `extends: ../../../DISCOVERY.Engine/Docker/plugin-base.yml`; the engine's compose `include:`s this file (`x-apk-plugin: true`) | Customer–Supplier (engine supplies the base service) — by sibling file path, not via the SDK's published `docker/plugin-base.yml` |
| DATABASE.volume.Log-STORAGE | named volume `apk-audio-logs` bound to `Documentation/LOGS`, declared identically here | Shared mutable state (by design: one log volume per node) |
| NODE.BareMetal supervisor / `GET /plugins` | retained `…/incoming/api` written by the runner | Published Language |
| DockTor | container labels `audio.apk.purpose*`, `audio.apk.endpoint.*` | Published Language (labels read by `docker inspect`) |
| APK.plugin.protocol.MAC_LOOKUP | Cargo git dep pinned by `rev` (clock identity → OUI) | Customer–Supplier |
| Shell / Gui panels | `SRC/gui/ptp/ptp.json` via `panels = [ptp]` | Published Language |
| Python callers (feature `python`) | pyo3 module from `src/lib.rs`, `parser.rs`, `clock.rs` | Second public surface (Open Host) |

## Scorecard
| Category | Score (0–3) | Finding |
|---|---|---|
| Coupling & Cohesion | 3 | No source reach into siblings; the PTP reader comes from PLUGIN.RAVENNA by `rev` (readers only). Compose `extends` is the house pattern, kept by design; the copied volume block is an include. |
| Boundary Integrity | 3 | `libc`/AF_PACKET confined to `net.rs`; MQTT through the runner's `Session` + `unkept`; topics from contract builders. |
| Duplication Control | 3 | No local say/restate/report or session; one PTPv2 parser shared with AES67; the pyo3 shim's second parser is gone. |
| Testing & Contracts | 3 | 84 unit tests + rustfmt test; the presence document is checked against the contracts' DeviceRecord type and schema; SDK CI on every push. |
| Change management | 2 | SDK and CI pinned to `sdk-v0.5`; crate `0.1.0` untagged (owner). |
Score key: 3 meets the standard · 2 minor gaps · 1 significant gaps · 0 absent / violated.

## Findings
1. GOOD — the raw-socket boundary is one module: `libc` is used only in [SRC/src/net.rs](../../SRC/src/net.rs) (and the bench bin [SRC/src/bin/ptp-monitor.rs](../../SRC/src/bin/ptp-monitor.rs)); parsing (the shared `apkaudio_ravenna::ptp`, [SRC/src/v1.rs](../../SRC/src/v1.rs)) is pure and tested.
2. GOOD — counters and agent state go through `apkaudio_contracts::agent_counters` / `agent_state`, presence through `apk_plugin_runner::presence`, and every publish through `apk_plugin_runner::unkept` ([SRC/src/agent.rs](../../SRC/src/agent.rs)).
3. Needs owner — tag this crate's releases (crate `0.1.0`, no tags); the coordinator publishes them (TAG-REQUEST filed).


Delivered items are recorded in [🌍Publishing/DELIVERY.md](../%F0%9F%8C%8DPublishing/DELIVERY.md#delivered-from-the-2026-10-30-audit).
