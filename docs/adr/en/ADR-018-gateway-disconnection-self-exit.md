# ADR-018: Runtime and Embed Self-Exit After a Gateway Disconnection Timeout

> **Chinese source of truth**: [ADR-018](../zh/ADR-018-gateway-disconnection-self-exit.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Proposed

## Date

2026-06-26

## Decision Makers

Architecture discussion

---

## Context

The Gateway is the sole lifecycle manager of this process tree:

```
Desktop App (Tauri)
  └── acowork-gateway (process)
        ├── acowork-runtime (process) — one per agent
        └── acowork-embed   (process) — one global
```

Today only two cases are handled. On **normal exit** (ctrl_c) only Embed is killed,
never Runtime. On **abnormal exit** (panic / SIGKILL / TerminateProcess) nothing is
cleaned up and both become orphans.

The normal path is being fixed elsewhere by having the Gateway kill them, but
**abnormal exit can never be handled by the Gateway — the process is already dead**
and cannot run cleanup code. So Runtime and Embed MUST be able to detect that the
Gateway is gone and exit on their own, as a last line of defence.

Current behaviour:

- **Runtime** — after the gRPC stream drops, `recv_message()` returns `Ok(None)`,
  triggering `try_reconnect_gateway()` → `reconnect_and_reregister()` with exponential
  backoff (from 100ms, capped at 30s) and a 300s total timeout, after which
  `LoopAction::Break` exits. It does exit, but takes at least 300s while holding
  memory and network resources.
- **Embed** — never connects to the Gateway at all. It is a standalone HTTP service that
  pushes heartbeats out over `/events` SSE (every 2s) but **consumes nothing back**, so
  it cannot know whether the Gateway is alive. It exits only on an OS signal
  (SIGTERM/SIGINT). **Once started it never exits**, which is the worst of the three cases.

## Goals

1. When the Gateway **crashes**, Runtime and Embed exit within a bounded time
   (e.g. 300s) and release their resources.
2. When the Gateway **restarts normally** (e.g. an upgrade), Runtime reconnects
   inside the window instead of exiting on a false timeout.
3. Keep the mechanism simple — no distributed consensus protocol.

## Alternatives

**A — the Gateway distributes an expected heartbeat interval; each side detects independently.**
At AgentHello / Embed registration the Gateway sends `heartbeat_interval_secs` and
`missed_heartbeat_limit`; each side watches the other and exits on timeout.

- Runtime starts a 300s countdown when the gRPC connection drops; a successful
  reconnect cancels it, expiry calls `exit(0)`.
- Embed polls Gateway `/health` every 10s and exits after 300s of consecutive failure.
- The Gateway side needs no change — `embed_supervisor` already detects Embed liveness via
  the 2s SSE heartbeat with a 10s timeout.

Covers both normal and abnormal exit, the timeout is configurable, and it does not
depend on OS signals. The cost is new Gateway health-probing logic in Embed, and
Runtime must move from unbounded reconnecting to bounded-then-exit.

**B — strengthen only the Gateway normal-exit cleanup, keep the Runtime reconnect as is.**
The Gateway kills all Runtime and Embed on normal exit; Runtime keeps its 300s
reconnect; Embed relies on OS signals or the active kill.

Smallest change and it fully covers the normal path, but on a crash Runtime still holds
resources for 300s and **Embed is never killed at all** — the worst leak.

**C — graceful timeout in Runtime/Embed plus a Gateway shutdown API.**
The Gateway adds `POST /api/shutdown`; Desktop prefers it before killing the Gateway
process; Runtime keeps its 300s reconnect; Embed gains the health probe from A.

Three layers of defence with faster release, but a much wider change and a new HTTP
endpoint.

## Decision

Adopt **Option A**.

**Rationale**

1. **Option B is unsafe** — Embed would survive a Gateway crash permanently, which is
   unacceptable for a long-running memory leak.
2. **A is simpler than C** — no new shutdown endpoint, and each side detects
   independently, which reduces coupling.
3. **Making the timeout a parameter** leaves Runtime enough reconnect window for a
   normal restart.

### Runtime

In `core/acowork-runtime/src/grpc/client.rs`, add a `disconnect_timeout_ms` field to
`GatewayGrpcClient` (default 300000). When `recv_message()` sees the connection drop, start
a countdown:

```
[gRPC disconnected] → [start 300s timer]
                        ├── [reconnected within 300s] → cancel timer, continue
                        └── [300s elapsed] → tracing::error + std::process::exit(1)
```

The Gateway supplies the value in `AgentHelloResult`; `AgentHelloConfig` gains the
matching field. `try_reconnect_gateway` still retries for 300s, but instead of
returning `LoopAction::Continue` past that point it exits the process.

### Embed

In `core/acowork-embed/src/main.rs`, add startup arguments:

- `--gateway-health-url` — the Gateway health endpoint, e.g. `http://127.0.0.1:19876/health`
- `--gateway-health-timeout-ms` — consecutive failure timeout (default 300000)
- `--gateway-health-interval-ms` — probe interval (default 10000)

When the URL is supplied, a background task polls `/health`:

```
[tick every 10s]
  └── GET /health
        ├── success → reset failure_count = 0
        └── failure → failure_count += 1
                    ├── failure_count * 10s < 300s  → continue
                    └── failure_count * 10s >= 300s → tracing::error + std::process::exit(1)
```

### Gateway

`spawn_embed_process()` (in `lifecycle/embed.rs`) passes its own health endpoint via
`--gateway-health-url`. The ctrl_c branch in `gateway/mod.rs` concurrently performs the
normal-exit cleanup (killing every Runtime and Embed) as the active defence.

### Defaults

| Parameter | Runtime | Embed |
|-----------|---------|-------|
| Detection | gRPC stream drop | HTTP GET /health |
| Timeout | 300s | 300s |
| Probe interval | n/a (passive) | 10s |
| Reconnect | exponential backoff within 300s | none |

## Impact

**Upside**

- After a Gateway crash, Runtime and Embed exit within 300s and free their memory.
- On normal exit the active kill releases resources immediately, without waiting.
- The timeout is pushed from Gateway config, so it can be tuned per deployment.

**Downside**

- Embed gains a dependency on the Gateway; running Embed standalone (no-Gateway mode)
  requires disabling the probe.
- The three crates must be version-released together.

**Mitigation**

- `--gateway-health-url` is optional; without it no probing starts.
- `disconnect_timeout_ms` defaults to 300s and is overridable via `AgentHelloResult`.
- On a normal restart the Gateway starts listening on its health endpoint before Embed starts,
  avoiding a startup race.

## Rollout

1. **Phase 1 (this ADR)** — Gateway normal-exit cleanup (kill Runtime + Embed) plus the
   self-exit mechanism above.
2. **Phase 2** — observe stability, then decide whether to adjust the default timeout or
   add adaptive reconnect windows.

## Open questions

- Should a network blip that recovers inside the 300s window reset the Runtime reconnect
  timer? The current design does (each new gRPC stream restarts the count).
- If the Gateway answers non-2xx (e.g. 503), should Embed count that as a failure?
  Recommended: **no** — the Gateway is alive, merely busy.
