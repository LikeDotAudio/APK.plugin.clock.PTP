# APK:plugin:PTP

PTP v1 + v2 + gPTP listener, one container — the other holding NET_RAW.

| | |
|---|---|
| **Recipe** | `Docker/docker-compose.yml` — no Dockerfile: it runs a binary from the shared plugin image built by `APK:discovery` |
| **Source** | `SRC/` (this repository — https://github.com/LikeDotAudio/APK.plugin.clock.PTP.git) — bin `apk-ptp`; `ptp-monitor` is the hand tool beside it; built by the shared plugin image |
| **Profile** | `l2` |
| **Talks to** | the broker only |

Was a thread in `workers/discovery.rs`. Moved into its own container by PLAN-3316.01.

---

*© 2026 — Written & Maintained by Anthony P. Kuzub. MIT Licence.*
