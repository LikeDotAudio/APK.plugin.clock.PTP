# Smoke test — PLUGIN.PTP

Is `PLUGIN.PTP` deployed? Written by `.apk.scripts/smoke.py` from the module's own shape; what each check means is in that script's header.

← [Wiki home](../README.md)

**✅ PASS** — 2/2 checks, since 2026-10-30 12:50 EDT

| Check | Target | Result | Detail |
|---|---|---|---|
| pushed | [APK.plugin.clock.PTP](https://github.com/LikeDotAudio/APK.plugin.clock.PTP.git) | ✅ | HEAD is on origin |
| container | `Plugin-PTP` | ✅ | running, healthy |

Re-run: `python3 .apk.scripts/smoke.py PODS/POD.THICK/PLUGINS/PLUGIN.PTP`

<!-- smoke:sig 3c4ac4f89baf since 2026-10-30 12:50 EDT -->
