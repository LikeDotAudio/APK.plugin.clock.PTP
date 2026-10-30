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

* No CI builds or tests it; the tests run by hand in the workspace.
* No release artefact or version: the image is built locally on each node (`up -d --build`).
* `ptp-monitor` is built into the image (`/usr/local/bin`) but has no file capabilities there; run by hand it needs `sudo` or `setcap cap_net_raw,cap_net_bind_service+eip`.
* The `python` feature (pyo3 shims `clock`, `parser`) is built by nothing; `cargo check -p apkaudio-ptp --features python` is the only check.
