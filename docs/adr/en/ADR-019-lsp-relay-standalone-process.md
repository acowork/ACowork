# ADR-019: Decoupling LSP Relay from the Gateway into a Standalone Process

> **Chinese source of truth**: [ADR-019](../zh/ADR-019-lsp-relay-standalone-process.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft (pending decision)

## Date

2026-07-01

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

**Phase 0 — extracting shared modules (an `acowork-core` extension)**:
`core/acowork-core/src/event_bus.rs` (**new**, generalized from
`acowork-embed/src/event_bus.rs`); `core/acowork-core/src/shutdown.rs` (**new**, moved from
`acowork-embed/src/shutdown.rs`); `core/acowork-core/src/supervisor.rs` (**new**, general blocks
extracted from `embed_supervisor.rs`); `core/acowork-core/src/health.rs` (**new**, defines the
`/health` + `/events` endpoint contract);
`core/acowork-embed/src/event_bus.rs` (**becomes** `type EmbedEventBus = EventBus<EmbedState>`);
`core/acowork-embed/src/shutdown.rs` (**deleted**, replaced by `use acowork_core::shutdown`);
`core/acowork-gateway/src/lifecycle/embed_supervisor.rs` (**refactored** to use the
`acowork_core::supervisor` blocks).

**Phases 1–3 — LSP Relay as a standalone process**:
`core/acowork-gateway/src/lsp/mod.rs` (1752 lines, **moved out wholesale**);
`core/acowork-gateway/src/lsp/pool.rs` (352 lines, **moved out wholesale**);
`core/acowork-gateway/src/http/routes.rs` (delete the 5 LSP routes + the `AppState.lsp_pool`
field); `core/acowork-gateway/src/http/server.rs` (delete the `start_reaper` call);
`core/acowork-gateway/src/config.rs` (delete the `lsp_config_dir` field);
`core/acowork-gateway/src/cli.rs` (delete the `--lsp-config-dir` argument);
`core/acowork-gateway/src/gateway/mod.rs` (add the LSP Relay supervisor startup logic);
`core/acowork-gateway/src/gateway/state.rs` (add the `lsp_relay_process` state field);
`core/acowork-lsp-relay/` (**a new crate**, carrying the moved LSP logic);
`apps/acowork-desktop/src-tauri/` (Monaco connects directly to LSP Relay, no longer via the
Gateway); `core/acowork-runtime/` (the codebase tool connects directly to LSP Relay).

---

## Context

### Problem 1: the LSP module is over-coupled to the Gateway

The Gateway design doc (`docs/design/zh/04-gateway.md`) states its positioning explicitly:

> The Gateway **does not proxy the agent's business logic** (it does not proxy LLM calls, it does
> not proxy tool execution); it only handles the coordination work that must be centralized.

The Gateway's core responsibilities are Package Manager, Lifecycle Manager, Intent Router, Key
Vault, Budget Tracker and Rate Limiter. What these have in common is **global resource management
and coordination**, not any concrete business protocol.

However, the current LSP module (`core/acowork-gateway/src/lsp/`, 2104 lines total, 6.5% of
Gateway code) takes on a large amount of work inconsistent with the Gateway's positioning:

| Responsibility | Lines | Conflict with the Gateway's positioning |
|---|---|---|
| LSP process pool management (spawn / reap / idle timeout) | ~350 | effectively a second Lifecycle Manager |
| WebSocket ↔ stdin/stdout bidirectional relay | ~400 | protocol proxying, not pass-through |
| LSP protocol state management (initialize handshake caching, JSON-RPC id substitution) | ~200 | deeply parsing and mutating business protocol messages |
| Install script execution (a 15-minute timeout) | ~200 | running heavy external scripts on the Gateway's tokio runtime |
| Command runnability validation (a two-stage probe) | ~150 | business logic |
| Configuration management (loading `lsp_servers.json`, built-in defaults) | ~300 | business configuration |
| HTTP API (5 routes) | ~200 | business endpoints |

### Problem 2: the stability risk has empirical evidence

Running the LSP install scripts once blocked the Gateway's tokio runtime, starving the embed
watchdog and wrongly killing the embed process (the full post-mortem is archived at
`docs/_internal/archive/plan/embed-heartbeat-timeout-fix.md`, for local reading). The root cause is
that LSP install scripts may run `npm install`, `pip install`, `cargo install` and other heavy
operations, whose nondeterminism conflicts with the Gateway's high availability requirements.

### Problem 3: the future codebase tool depends strongly on LSP

The coding agent's codebase tool needs to call the LSP protocol to obtain code intelligence:

```
Agent Runtime (codebase tool)
    │
    ├── textDocument/definition     → needs LSP
    ├── textDocument/references     → needs LSP
    ├── textDocument/hover          → needs LSP
    ├── workspace/symbol            → needs LSP
    └── textDocument/diagnostic     → needs LSP
```

If LSP lived in the Desktop App frontend, every codebase tool call from the Agent Runtime would
have to be relayed through the frontend — architecturally unacceptable: the Agent Runtime is a
backend process and cannot depend on whether the frontend happens to be open. **LSP must be shared
backend infrastructure.**

### Problem 4: the Gateway is genuinely transparent for IPC but deeply parses LSP

The Gateway's gRPC IPC to the Agent Runtime is true protocol pass-through — it does not parse
message content, it only routes. The LSP relay, however, deeply parses and mutates JSON-RPC
messages: `is_initialize_request` / `is_initialized_notification` / `is_initialize_result` (an LSP
protocol state machine); `substitute_jsonrpc_id` (JSON-RPC id substitution); `extract_method_hint`
(parsing the LSP method field); and the initialize handshake cache
(`init_result: Mutex<Option<String>>`).

This inconsistency undermines the Gateway's architectural simplicity.

## Goals

1. The Gateway leaves the LSP data path entirely: no WebSocket proxying, no LSP process pool
   management, no JSON-RPC parsing
2. LSP runs as a standalone process; the Gateway only spawn / monitors / restarts it (reusing the
   embed supervisor pattern)
3. The Desktop App (Monaco) and the Agent Runtime (the codebase tool) connect directly to LSP
   Relay, not through the Gateway
4. An LSP Relay crash does not affect Gateway stability, and after a Gateway crash the LSP Relay
   can exit by itself

## Options

### Option A: a standalone process (`acowork-lsp-relay`) — recommended

**Principle**: move the whole LSP module out of the Gateway into a standalone binary
`acowork-lsp-relay`. The Gateway manages its lifecycle through the supervisor pattern
(spawn / monitor / restart), exactly as it does for embed.

```
┌──────────────────────────────────────────────────────────┐
│                    Gateway (after slimming)               │
│                                                          │
│  ┌─────────────┐  ┌──────────┐  ┌────────────────────┐  │
│  │ Package Mgr │  │Lifecycle │  │ LSP Relay          │  │
│  │             │  │ Manager  │  │ Supervisor         │  │
│  ├─────────────┤  ├──────────┤  │ spawn/monitor/     │  │
│  │ Key Vault   │  │ Intent   │  │ restart            │  │
│  │             │  │ Router   │  └─────────┬──────────┘  │
│  ├─────────────┤  ├──────────┤            │              │
│  │ Budget      │  │ Rate     │   GET /api/lsp/endpoint  │
│  │ Tracker     │  │ Limiter  │   returns the LSP Relay address │
│  └─────────────┘  └──────────┘                          │
└──────────────────────────────────────────────────────────┘
                    │ spawn + SSE heartbeat
                    ▼
┌──────────────────────────────────────────────────────────┐
│              acowork-lsp-relay (standalone process)      │
│                                                          │
│  ┌──────────────────┐  ┌──────────────────────────────┐  │
│  │ WebSocket Server │  │ JSON-RPC API                 │  │
│  │ /lsp/:language   │  │ /api/codebase/*              │  │
│  │ (Monaco direct)  │  │ (Agent Runtime codebase tool)│  │
│  ├──────────────────┤  ├──────────────────────────────┤  │
│  │ LSP Process Pool │  │ Install Scripts + Status     │  │
│  │ (spawn/reap)     │  │ /api/lsp/install/*           │  │
│  ├──────────────────┤  ├──────────────────────────────┤  │
│  │ Config           │  │ Health                       │  │
│  │ lsp_servers.json │  │ /health + SSE /events        │  │
│  └──────────────────┘  └──────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
        ▲                           ▲
        │ WebSocket direct          │ JSON-RPC direct
        │                           │
┌───────┴────────┐          ┌───────┴────────┐
│  Desktop App   │          │  Agent Runtime │
│  (Monaco)      │          │ (codebase tool)│
└────────────────┘          └────────────────┘
```

**Advantages**:
- Complete isolation: an LSP crash does not affect the Gateway, and after a Gateway crash the LSP
  Relay exits on its own via the supervisor timeout
- Reuse of a proven pattern: the embed supervisor has already solved process discovery, health
  checks, crash recovery, PID-aware reaping and the startup grace window
- Greatly slimmed Gateway: 2104 lines of LSP code + 5 routes + the AppState field + the Config
  field + the CLI argument all deleted
- Direct client connections: Monaco and the codebase tool connect straight to LSP Relay, the
  Gateway is not on the data path, zero performance overhead
- Independent evolution: LSP Relay can be upgraded independently, have its resource limits
  configured independently, and choose its own tokio runtime parameters

**Disadvantages**: one more crate and binary (build complexity); the Desktop App must first query
the Gateway for the LSP Relay port before connecting (one extra HTTP request); and the version
releases of three components must be coordinated.

### Option B: a separate crate, still inside the Gateway process

**Principle**: extract the LSP logic into an `acowork-lsp-relay` crate but still run it as a
library inside the Gateway process.

**Advantages**: code isolation without a new process; a smaller change.

**Disadvantages**: does not solve runtime isolation — LSP still runs on the Gateway's tokio
runtime; does not solve resource contention — the LSP process pool still consumes the Gateway's
memory and CPU; does not solve the blocking-install-script problem. It is essentially code
reshuffling, not architectural decoupling.

### Option C: move it to the Desktop App (Tauri side)

**Principle**: the LSP relay runs as a Tauri sidecar or an embedded service.

**Advantages**: the Gateway is entirely unaffected.

**Disadvantages**: the Agent Runtime's codebase tool cannot use LSP (the frontend may not even be
open); the LSP process lifecycle is bound to the Desktop App rather than to a system service; and
it contradicts the architectural direction of the future coding agent.

## Decision

Adopt **Option A: a standalone process (`acowork-lsp-relay`)**.

### Rationale

1. **Option B does not solve the fundamental problem**: code isolation is not runtime isolation,
   and the LSP install scripts can still block the Gateway runtime
2. **Option C contradicts the future direction**: the codebase tool needs LSP as backend
   infrastructure and cannot depend on the frontend
3. **Option A reuses a proven pattern**: the embed supervisor has already validated
   "standalone process + Gateway supervisor", and LSP Relay can reuse the same pattern directly
4. **Option A is the only one satisfying both "Gateway stability" and "codebase availability"**

## Preliminary work: extracting shared modules (Phase 0, high priority)

Before creating `acowork-lsp-relay`, the patterns shared between embed and LSP Relay must be
extracted into `acowork-core` to avoid code duplication. As "standalone subprocesses managed by the
Gateway", they share the following infrastructure needs:

### Shared pattern analysis

| # | Module | Current location | Lines | Reused by | Reuse value |
|---|---|---|---|---|---|
| 1 | **EventBus** — broadcast channel + heartbeat + SSE event model | `acowork-embed/src/event_bus.rs` | 113 | embed, LSP relay | **high** — both need an SSE heartbeat for supervisor monitoring |
| 2 | **Shutdown** — cross-platform signal handling (SIGTERM / SIGINT / Ctrl+C) | `acowork-embed/src/shutdown.rs` | 77 | embed, LSP relay | **high** — both need graceful exit |
| 3 | **Supervisor blocks** — RestartHistory, exponential backoff, SSE frame parsing, heartbeat watchdog | `embed_supervisor.rs` | ~400 | embed supervisor, LSP relay supervisor | **high** — the two supervisors' logic is nearly identical |
| 4 | **Health endpoint contract** — the response format of `/health` and `/events` | an implicit convention | — | embed, LSP relay | **medium** — a unified contract reduces supervisor differences |
| 5 | **Idle timeout** — sub-process no-output timeout | `acowork-core/src/process.rs` | already present | LSP install, embed download | **already available** — reuse directly, no change needed |

### Extraction design

#### 1. `acowork-core::event_bus` — the generic event bus (new)

Generalize `acowork-embed/src/event_bus.rs`, replacing the embed-specific `State` enum with a
generic parameter:

```rust
// core/acowork-core/src/event_bus.rs

use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;

/// Generic event flowing over the bus.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BusEvent<S: Clone + Serialize> {
    /// Periodic liveness signal.
    Heartbeat { seq: u64 },
    /// Application-level state transition.
    State { seq: u64, state: S },
}

/// Bus for broadcasting events to all subscribers.
///
/// Uses `tokio::sync::broadcast` internally. Each new subscriber starts
/// receiving events from the moment of subscription onwards.
///
/// # Type parameter
///
/// `S` is the application-specific state type. For embed it's `EmbedState`
/// (Starting, Loading, Ready, Error); for LSP relay it's `LspRelayState`
/// (Starting, Ready, Error).
#[derive(Clone)]
pub struct EventBus<S: Clone + Serialize + Send + Sync + 'static> {
    tx: broadcast::Sender<Arc<BusEvent<S>>>,
    seq: Arc<AtomicU64>,
}

impl<S: Clone + Serialize + Send + Sync + 'static> EventBus<S> {
    pub fn new(buffer: usize) -> Self { /* ... */ }
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BusEvent<S>>> { /* ... */ }
    pub fn publish_state(&self, state: S) -> u64 { /* ... */ }
    pub fn spawn_heartbeat(&self, interval_ms: u64) { /* ... */ }
}
```

**embed adaptation**: `type EmbedEventBus = EventBus<embed::State>;`
**LSP relay adaptation**: `type LspRelayEventBus = EventBus<LspRelayState>;`

#### 2. `acowork-core::shutdown` — generic graceful exit (new)

Moved from `acowork-embed/src/shutdown.rs` with no generalization needed (the logic is entirely
generic):

```rust
// core/acowork-core/src/shutdown.rs

pub struct Shutdown { flag: AtomicBool }
impl Shutdown {
    pub fn new() -> Arc<Self> { /* ... */ }
    pub fn is_shutting_down(&self) -> bool { /* ... */ }
    pub fn request(&self) { /* ... */ }
}
pub fn install_signal_handlers(shutdown: Arc<Shutdown>) { /* ... */ }
```

#### 3. `acowork-core::supervisor` — supervisor blocks (new)

Extract the embed-agnostic logic from `embed_supervisor.rs`:

```rust
// core/acowork-core/src/supervisor.rs

/// Tracks consecutive restart attempts within a sliding window.
pub struct RestartHistory { /* ... */ }
impl RestartHistory {
    pub fn new() -> Self { /* ... */ }
    pub fn record(&mut self, window: Duration) -> usize { /* ... */ }
}

/// Compute exponential backoff with ±20% jitter, clamped to [min, max].
pub fn backoff_with_jitter(attempt: u32, min: Duration, max: Duration) -> Duration { /* ... */ }

/// Minimal SSE frame parser. Returns `None` for comments or unparseable frames.
pub fn parse_sse_frame(frame: &str) -> Option<SseFrame> { /* ... */ }

pub enum SseFrame {
    Heartbeat,
    State(String),  // raw JSON payload — the caller deserializes
    Comment(String),
}

/// Heartbeat watchdog: wraps a tokio::time::Interval and checks elapsed
/// time since the last heartbeat. Returns `true` when timeout exceeded.
pub struct HeartbeatWatchdog {
    interval: tokio::time::Interval,
    last_heartbeat: Instant,
    timeout: Duration,
}
impl HeartbeatWatchdog {
    pub fn new(check_interval: Duration, timeout: Duration) -> Self { /* ... */ }
    /// Wait for the next tick, then check if the heartbeat is stale.
    pub async fn tick(&mut self) -> HeartbeatStatus { /* ... */ }
    /// Call on every received heartbeat to reset the timer.
    pub fn beat(&mut self) { /* ... */ }
}

pub enum HeartbeatStatus {
    Ok,
    Timeout { elapsed_secs: u64 },
}
```

#### 4. `acowork-core::health` — the health/events endpoint contract (new)

Define the standard contract between the Gateway supervisor and the managed subprocesses:

```rust
// core/acowork-core/src/health.rs

/// Standard health check response that every Gateway-managed subprocess
/// MUST return from `GET /health`.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    /// "ok" | "degraded" | "starting"
    pub status: String,
    /// Process version (from CARGO_PKG_VERSION)
    pub version: String,
    /// Process name for diagnostics (e.g. "acowork-embed", "acowork-lsp-relay")
    pub process: String,
    /// Process-specific payload (model info for embed, language count for LSP relay)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Standard SSE event names used by the supervisor.
pub mod sse_event {
    pub const HEARTBEAT: &str = "heartbeat";
    pub const STATE: &str = "state";
}

/// Recommended constants for supervisor configuration.
pub mod supervisor_defaults {
    use std::time::Duration;
    pub const HEARTBEAT_INTERVAL_MS: u64 = 2000;
    pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
    pub const STARTUP_GRACE: Duration = Duration::from_secs(10);
    pub const STARTUP_POLL: Duration = Duration::from_secs(2);
    pub const RESTART_BACKOFF_MIN: Duration = Duration::from_secs(1);
    pub const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(60);
    pub const RESTART_WINDOW: Duration = Duration::from_secs(5 * 60);
    pub const MAX_RESTART_ATTEMPTS: u32 = 5;
}
```

### The crate dependency relationships after extraction

```
acowork-core (adds event_bus, shutdown, supervisor, health)
    ▲                    ▲
    │                    │
acowork-embed      acowork-lsp-relay
(uses EventBus<EmbedState>,     (uses EventBus<LspRelayState>,
 Shutdown, the /health contract)  Shutdown, the /health contract)

acowork-gateway
(uses the supervisor blocks: RestartHistory, backoff_with_jitter,
 HeartbeatWatchdog, parse_sse_frame, supervisor_defaults)
```

### Implementation priority

| Stage | Content | Priority | Rationale |
|---|---|---|---|
| **Phase 0a** | `acowork-core::shutdown` | **P0** | no dependencies, 77 lines, embed switches over directly |
| **Phase 0b** | `acowork-core::event_bus` | **P0** | once generalized both embed and LSP relay can use it |
| **Phase 0c** | `acowork-core::health` | **P0** | defines the contract so later implementations have a basis |
| **Phase 0d** | `acowork-core::supervisor` | **P1** | depends on the first three; used by the Gateway supervisor refactor |
| **Phase 1** | create `acowork-lsp-relay` | P1 | depends on Phase 0 |
| **Phase 2** | Gateway integrates the supervisor | P2 | depends on Phase 1 |
| **Phase 3** | switch clients + cleanup | P3 | depends on Phase 2 |

### Detailed design

#### 1. The new crate `core/acowork-lsp-relay/`

```
core/acowork-lsp-relay/
├── Cargo.toml
└── src/
    ├── main.rs              # entry: parse the CLI, init EventBus + Shutdown, start the HTTP+WS server
    ├── lib.rs               # library entry
    ├── state.rs             # the LspRelayState enum (Starting, Ready, Error)
    ├── config.rs            # lsp_servers.json loading (moved in from the Gateway)
    ├── pool.rs              # the LSP process pool (moved in from the Gateway, unchanged)
    ├── relay.rs             # the WebSocket ↔ stdin/stdout relay (moved in from the Gateway)
    ├── protocol.rs          # LSP protocol helpers (initialize caching, JSON-RPC id substitution)
    ├── install.rs           # install script management (moved in; uses acowork_core::process::run_command_with_idle_timeout)
    ├── codebase.rs          # the JSON-RPC API for the Agent Runtime codebase tool (new)
    ├── server.rs            # the Axum HTTP + WebSocket server (uses acowork_core::event_bus::EventBus<LspRelayState>)
    └── health.rs            # /health (returns acowork_core::health::HealthResponse) + SSE /events
```

**Dependencies**: `acowork-core` (event_bus, shutdown, health, process, logging); `axum` + `tokio` +
`serde_json` + `tracing`; **no dependency on `acowork-gateway`**.

**CLI arguments**:

```rust
#[derive(Parser)]
struct Cli {
    /// HTTP listen address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP listen port (0 = auto-assign)
    #[arg(long, default_value = "0")]
    port: u16,

    /// LSP config directory
    #[arg(long)]
    lsp_config_dir: Option<String>,

    /// Gateway health URL (used for self-exit detection)
    #[arg(long)]
    gateway_health_url: Option<String>,

    /// Gateway disconnect timeout (ms)
    #[arg(long, default_value = "300000")]
    gateway_health_timeout_ms: u64,

    /// Gateway health probe interval (ms)
    #[arg(long, default_value = "10000")]
    gateway_health_interval_ms: u64,
}
```

#### 2. The Gateway side

**Deleted**: `core/acowork-gateway/src/lsp/mod.rs` (1752 lines);
`core/acowork-gateway/src/lsp/pool.rs` (352 lines); the `AppState.lsp_pool` field; the 5 LSP HTTP
routes (`/lsp/{language}`, `/api/lsp/servers`, `/api/lsp/status`, `/api/lsp/install/{language}`
GET/POST); the `LspPool::start_reaper` call in `server.rs`; the `lsp_config_dir` field in
`config.rs`; and the `--lsp-config-dir` argument in `cli.rs`.

**Added** — `core/acowork-gateway/src/lifecycle/lsp_relay.rs` (modelled on `embed.rs`):

```rust
/// LSP Relay process state
#[derive(Debug, Clone)]
pub struct LspRelayProcessState {
    pub pid: u32,
    pub port: u16,
    pub ready: bool,
}

/// Spawn the acowork-lsp-relay process
pub async fn spawn_lsp_relay(
    lsp_config_dir: Option<&str>,
    port: u16,
    gateway_health_url: &str,
) -> Result<(LspRelayProcessState, tokio::process::Child), GatewayError>;

/// Kill the LSP Relay process
pub async fn kill_lsp_relay(pid: u32) -> Result<(), GatewayError>;
```

`core/acowork-gateway/src/lifecycle/lsp_relay_supervisor.rs` (modelled on
`embed_supervisor.rs`):

```rust
/// Start the LSP Relay supervisor.
///
/// Monitors the LSP Relay's SSE /events stream, detects heartbeat timeouts (10s),
/// and restarts with exponential backoff after a crash (cap 5 times / 5 minutes).
pub fn start_lsp_relay_supervisor(
    cfg: LspRelaySupervisorConfig,
    state: SharedState,
);
```

`core/acowork-gateway/src/gateway/state.rs`:

```rust
pub struct GatewayState {
    // ... existing fields ...
    /// LSP Relay process state (None if not started)
    pub lsp_relay_process: Option<LspRelayProcessState>,
}
```

`core/acowork-gateway/src/http/routes.rs`:

```rust
/// GET /api/lsp/endpoint — returns the LSP Relay's address
///
/// The Desktop App and the Agent Runtime discover the LSP Relay through this
/// endpoint, then connect directly to its WebSocket and JSON-RPC API.
pub async fn lsp_endpoint(State(state): State<AppState>) -> Json<LspEndpointResponse> {
    let gw = state.gateway_state.read().await;
    match &gw.lsp_relay_process {
        Some(eps) if eps.ready => Json(LspEndpointResponse {
            available: true,
            host: "127.0.0.1".to_string(),
            port: Some(eps.port),
        }),
        _ => Json(LspEndpointResponse {
            available: false,
            host: "127.0.0.1".to_string(),
            port: None,
        }),
    }
}
```

#### 3. The Desktop App side

The Monaco Editor connection flow changes:

```
old flow:
  Monaco → WebSocket ws://127.0.0.1:19876/lsp/rust
           (relayed through the Gateway)

new flow:
  1. GET http://127.0.0.1:19876/api/lsp/endpoint → { port: 19878 }
  2. Monaco → WebSocket ws://127.0.0.1:19878/lsp/rust
              (direct to LSP Relay)
```

The LSP install/status UI likewise switches to connecting directly to LSP Relay.

#### 4. The Agent Runtime side

The codebase tool obtains the LSP Relay address as follows:

```
new flow:
  1. AgentHelloResult gains an lsp_relay_endpoint field
  2. The codebase tool obtains the LSP Relay address via gRPC
  3. codebase tool → JSON-RPC http://127.0.0.1:{port}/api/codebase/definition
```

#### 5. Lifecycle management

```
Gateway startup:
  1. spawn acowork-lsp-relay (port auto-assigned or fixed)
  2. wait for LSP Relay /health readiness (a 10s startup grace)
  3. start the LSP Relay supervisor (SSE heartbeat monitoring)
  4. write the LSP Relay state into GatewayState.lsp_relay_process

Normal Gateway operation:
  - the supervisor monitors the SSE heartbeat (a 2s interval, a 10s timeout)
  - crash → restart with exponential backoff (1s, 2s, 4s, 8s, ... max 60s)
  - the 5 times / 5 minutes cap → give up and mark unavailable

Normal Gateway shutdown:
  1. kill the LSP Relay process
  2. wait for the child to exit
  3. the Gateway itself exits

Abnormal Gateway crash:
  - LSP Relay detects that gateway_health_url is unreachable
  - exits with exit(0) after a 300s timeout (reusing the ADR-018 pattern)
```

#### 6. Port allocation strategy

| Strategy | Description |
|---|---|
| default | `--port 0`, the OS assigns it; the actual port is returned by `/api/lsp/endpoint` |
| fixed | `--port 19878`, for debugging and fixed deployments |
| conflict handling | the same port-increment strategy as the Gateway HTTP server |

### Comparison with the embed supervisor pattern

| Dimension | embed | LSP Relay |
|---|---|---|
| standalone binary | `acowork-embed` | `acowork-lsp-relay` |
| Gateway responsibility | spawn / monitor / restart | spawn / monitor / restart |
| health monitoring | SSE heartbeat (a 2s interval, a 10s timeout) | SSE heartbeat (a 2s interval, a 10s timeout) |
| crash recovery | exponential backoff, a 5 times / 5 minutes cap | exponential backoff, a 5 times / 5 minutes cap |
| state storage | `GatewayState.embed_process` | `GatewayState.lsp_relay_process` |
| port discovery | fixed 18080 | auto-assigned or fixed |
| shutdown | Gateway shutdown → kill the child | Gateway shutdown → kill the child |
| self-exit | ADR-018: Gateway health probe, a 300s timeout | ADR-018: Gateway health probe, a 300s timeout |
| optionality | falls back to remote embedding when unavailable | when unavailable the LSP feature is unavailable (no fallback) |

## Impact

**Positive impact**: the Gateway's code volume drops by ~2104 lines (6.5%) and its complexity
falls significantly; an LSP crash no longer threatens Gateway stability; LSP install scripts run in
an independent process and cannot block the Gateway runtime; the Gateway's LSP responsibility is
simplified from "protocol proxy + process pool management + install execution" to
"spawn / monitor / restart"; the codebase tool gains directly usable LSP backend infrastructure;
and LSP Relay can be upgraded and resource-limited independently.

**Negative impact**: one more crate and binary, increasing build time; one extra HTTP request for
the Desktop App (to obtain the LSP Relay port); the version releases of three components (Gateway,
LSP Relay, Desktop App) must be coordinated; and when LSP Relay is unavailable there is no
fallback (unlike embed, which can fall back to remote).

**Mitigations**: the LSP Relay port can be fixed and the Desktop App can cache it to avoid
querying every time; the LSP Relay is an optional component — if the binary does not exist the
Gateway skips starting it and the LSP feature is unavailable without affecting anything else; and
version coordination is handled by including the version number in the Gateway's
`/api/lsp/endpoint` response so the Desktop App can perform a compatibility check.

## Migration Path

Four phases; Phase 0 (shared module extraction) is the highest priority and precedes creating the
LSP relay.

### Phase 0: extract the shared modules into acowork-core (P0, first)

1. **Phase 0a**: `acowork-core::shutdown` — moved from `acowork-embed/src/shutdown.rs`, with embed
   switching to `use acowork_core::shutdown`
2. **Phase 0b**: `acowork-core::event_bus` — generalize `EventBus<S>`, with embed becoming
   `type EmbedEventBus = EventBus<EmbedState>`
3. **Phase 0c**: `acowork-core::health` — define `HealthResponse`, the SSE event name constants
   and the supervisor default parameters
4. **Phase 0d**: `acowork-core::supervisor` — extract `RestartHistory`, `backoff_with_jitter`,
   `HeartbeatWatchdog` and `parse_sse_frame`; the Gateway's `embed_supervisor.rs` switches to these
   blocks

### Phase 1: create the acowork-lsp-relay crate (non-breaking)

1. Create the `core/acowork-lsp-relay/` crate depending on `acowork-core` (using event_bus,
   shutdown, health)
2. Move the code from `lsp/mod.rs` and `lsp/pool.rs` into the new crate
3. Add `main.rs`, `server.rs` and `health.rs` (using
   `acowork_core::event_bus::EventBus<LspRelayState>`)
4. Keep the Gateway's existing LSP module unchanged (dual-track operation)

### Phase 2: Gateway integrates the supervisor

1. The Gateway adds `lifecycle/lsp_relay.rs` and `lifecycle/lsp_relay_supervisor.rs` (using the
   `acowork_core::supervisor` blocks)
2. The Gateway spawns the LSP Relay process on startup
3. Add the `GET /api/lsp/endpoint` endpoint
4. Verify that the LSP Relay's functionality is equivalent to the Gateway's in-process LSP

### Phase 3: switch clients + clean up the Gateway

1. The Desktop App's Monaco connects directly to LSP Relay
2. The Agent Runtime's codebase tool integrates with LSP Relay
3. Delete the Gateway's `lsp/` module, the LSP routes, `AppState.lsp_pool` and the LSP fields in
   Config/Cli
4. Clean up the loading paths for `lsp_servers.json` and `lsp_install/` (moved into LSP Relay)

## Unresolved Questions

- Does the LSP Relay need to support multiple instances? (Currently a single instance, consistent
  with embed)
- Should the LSP Relay's port be configurable in the Gateway's config file?
- If a user already runs a standalone LSP Relay (not managed by the Gateway), how does the Gateway
  discover and attach to it? (Refer to embed's `attach_existing_embed_process` pattern)
- Are the LSP Relay's logs a separate file, or are they collected centrally by the Gateway?
