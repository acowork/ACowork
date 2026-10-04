# dev/

Build, packaging, CI, and deployment scripts. **None of this ships in a
release artifact** — it exists so a developer can go from a clean clone to a
running stack, and so CI has one entry point per check.

A script earns a place here by being **reproducible**: a build step, a
packaging step, a CI gate, or a regression test tied to a design doc or ADR.
One-off probes, debug helpers, and ad-hoc experiments do not belong here — see
[What is deliberately absent](#what-is-deliberately-absent).

## Prerequisites

| Tool | Needed for |
|---|---|
| Rust (stable toolchain) | everything under `core/` |
| Node.js + npm | `apps/acowork-desktop`, `apps/acowork-mobile` |
| Python 3 + `pillow` | frontend smoke tests, store logos |
| PowerShell 7 (`pwsh`) | the `.ps1` scripts on Windows |

ONNX Runtime is **not** a manual prerequisite — `setup_ort.*` installs it into
`.ort/`, and `ort_env.js` makes Tauri auto-detect it on every platform.

## First build

```bash
./dev/setup_ort.sh                    # Linux / macOS
./dev/setup_ort.ps1                   # Windows
./dev/build_core.sh                   # or build_core.ps1 on Windows
```

`build_core.*` builds the Gateway, Runtime, and Node Agent in the release
profile. `-Debug` / `--debug` switches to the debug profile and auto-raises
`ACOWORK_GATEWAY_LOG_LEVEL` to `debug` for every process it spawns.

## Build and run

| Script | What it does |
|---|---|
| [`build_core.ps1`](build_core.ps1) · [`build_core.sh`](build_core.sh) | Build Gateway + Runtime + Node Agent. `-Start` stops the old Gateway and starts a new one; `-Start -Remote` binds `0.0.0.0` and advertises the detected LAN IP instead of `127.0.0.1` — this is how you make the Gateway reachable from a phone on the same network. |
| [`build_macos.sh`](build_macos.sh) | macOS one-click build: skips the slow `setup_ort.sh` download, pulls the Apple Silicon ONNX Runtime with `download-ort,coreml`, installs Homebrew `pkg-config`/`cmake`. |
| [`start_node.ps1`](start_node.ps1) · [`start_node.sh`](start_node.sh) | Start a local `acowork-node` daemon joined to a remote Gateway (`<host>:<port>`). |
| [`ort_env.js`](ort_env.js) | Invoked by `tauri.conf.json`'s `beforeDevCommand`; not run by hand. Probes `.ort/`, then the Cargo cache, and injects `--features download-ort[,coreml\|cuda\|directml]` when nothing is installed. |

## Packaging

| Script | What it does |
|---|---|
| [`package_desktop_windows.ps1`](package_desktop_windows.ps1) | Windows installer (`.msi` / NSIS bundle). `-ReinstallOrt`, `-NoMirror`. |
| [`package_desktop_macos.sh`](package_desktop_macos.sh) | macOS `.app` / `.dmg`. |
| [`package_desktop_linux.sh`](package_desktop_linux.sh) | Linux `.deb` / `.rpm` / AppImage. |

App icons are **not** generated here. `npx tauri icon assets/app-icon.svg`
produces the platform set, `scripts/generate-store-logos.py` adds the Windows
Store logos that `tauri icon` does not emit, and
`scripts/check-app-icon-fill.mts` fails if a regenerated icon's background
shrinks back inside the canvas.

## Agent packages

| Script | What it does |
|---|---|
| [`build-agent.ps1`](build-agent.ps1) · [`build-agent.sh`](build-agent.sh) | Zip an `.agent` directory into a signed package, generating signing keys on first use. Pass an agent directory, or use `-All` to build every agent under `examples/`. |

## CI and tests

[`ci.sh`](ci.sh) is the single entry point; `./dev/ci.sh all` is what runs
before a release.

| Mode | Runs |
|---|---|
| `check` | `cargo check --all` |
| `clippy` | `cargo clippy --all-targets -- -D warnings` |
| `test` | `cargo test` (unit + integration) |
| `integration` | cross-process suites (relay full path, PM remote) |
| `smoke` | frontend smoke suite — boots a real Gateway + Runtime and drives them over HTTP/MQTT exactly as the Desktop would |
| `all` | all of the above, in order |

Two red lines are enforced here and are not optional:

- `run_gateway_fs_redline` — the Gateway must not read agent-private data off
  the filesystem; it goes through Runtime HTTP only. Rationale and the frozen
  ceilings are in [ADR-009 §5](../docs/adr/zh/ADR-009-gateway-workspace-isolation.md).
- `acowork-node` must not depend on `acowork-gateway` (ADR-055 §6.20).

Tests that live outside the Cargo test harness:

| Test | What it covers |
|---|---|
| [`e2e_frontend_smoke/smoke_test.py`](e2e_frontend_smoke/smoke_test.py) | The `smoke` mode above. Covers the cases enumerated in `docs/plan/zh/e2e-frontend-smoke-test.md`, plus the PM member feature end-to-end across processes. |
| [`e2e_frontend_smoke/onboarding_installs_all_agents.py`](e2e_frontend_smoke/onboarding_installs_all_agents.py) | ADR-059 first-run bootstrap handshake regression. `GET /api/bootstrap` is the readiness source of truth, **not** `/health`. |
| [`e2e_stop_test.ps1`](e2e_stop_test.ps1) | Stop-response latency after the control/data channel split: open a WebSocket, stream a long message over HTTP, send `stop` on the first delta, measure the latency. Referenced by [ADR-077](../docs/adr/zh/ADR-077-system-agent-demotion-to-default-agent.md). |

## Deployment

[`deploy/relay/`](deploy/relay/) — self-hosting the relay server:

| File | What it does |
|---|---|
| [`acowork-relay.service`](deploy/relay/acowork-relay.service) | systemd unit. Add `--log-level debug` when diagnosing a wedged device tunnel: yamux logs its stream lifecycle and ping/pong RTT at debug, and that is the only evidence a stalled pipe leaves behind (design doc 24 §5.2). |
| [`certbot-wildcard.sh`](deploy/relay/certbot-wildcard.sh) | Wildcard certificate for `*.<relay-domain>` — the relay serves every device from its own subdomain, so one wildcard cert covers all of them. |

See the root [README § Relay server](../README.md) for the deployment walkthrough.

## What is deliberately absent

Scratch scripts do not live in version control. Keep one-off probes, debug
helpers, and ad-hoc experiments in `dev/tmp/` (git-ignored) or outside the
repository, and delete them once the investigation is over. `*.log` is ignored
as well.
