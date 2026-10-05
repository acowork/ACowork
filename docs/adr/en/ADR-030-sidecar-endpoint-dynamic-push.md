# ADR-030: Dynamic Push of Sidecar Endpoints — Gateway → Runtime

> **Chinese source of truth**: [ADR-030](../zh/ADR-030-sidecar-endpoint-dynamic-push.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

**Status**: Completed (C1 ✅ C2 ✅ C3 ✅ C4 ✅)
**Date**: 2026-07-08
**Decision Makers**: Dayu

**Predecessors**:
- ADR-019 (decoupling LSP Relay into a standalone process)
- ADR-029 (persistence and enablement control of builtin tools — agent_tools.json)

---

## Decision Summary

**4 commits, each independently buildable (the user chose "Path B")**:

| Commit | Scope | Status |
|--------|-------|------|
| **C1** | the `SidecarEndpointUpdate` message + the `SidecarKind` enum + the proto + the bridge + grpc client decoding (**purely additive**) | ✅ done at HEAD |
| **C2** | the Gateway `GlobalResourcePusher::push_sidecar_endpoint()` + **migrating the embed supervisor** (keeping `push_embedding_config()` as a deprecated wrapper) | ⏳ to do |
| **C3** | the Runtime `register_dynamic_tool()` / `unregister_dynamic_tool()` + routing `SidecarEndpointUpdate` in cli.rs + **wiring the LSP relay supervisor to the pusher** + enabling codebase by default in agent_tools.json | ⏳ to do |
| **C4** | **cleanup**: remove the `embed_config_json` field from `RuntimeConfigUpdate` + remove the `EmbeddingConfigUpdate` variant from `GatewayResponse` + remove the `push_embedding_config()` function | ⏳ to do |

**Key decisions** (in conversation order):

| Decision | Source | Content |
|------|------|------|
| No L1 Readiness Barrier | user 2026-07-08 05:23:36 | "It is unreasonable for the frontend/agent to wait deadlocked for these processes to be ready. The current agent hello is reasonable; the readiness of subprocesses should be handled separately" — AgentHello stays as it is; the pusher backfills asynchronously |
| A new standalone message rather than an embedded field | user 2026-07-08 06:20:38 | `SidecarEndpointUpdate` is a new variant of `GatewayResponse`, not stuffed into `RuntimeConfigUpdate` |
| embed is fully migrated over | user 2026-07-08 06:22:13 | "Upgrade straight through; the project is still in development and there are no compatibility requirements" — embed also goes through `SidecarEndpointUpdate`, and the old channel is removed at the end |
| The frontend is not changed for now | user 2026-07-08 06:20:38 | These 4 commits do not touch the Desktop App (the right-hand tool panel is filed as a separate item after C4) |
| Path B (4 commits) | user 2026-07-08 06:33:49 | "Let's go with B, one step at a time" |

---

## Blast Radius

### C1 (done)

**Added**:
- `core/acowork-core/proto/gateway_ipc.proto`: the `SidecarKind` enum + the `SidecarEndpointUpdate` message + a new tag 44 on `ServerMessage.payload`
- `core/acowork-core/src/protocol.rs`: the `SidecarKind` enum + the `GatewayResponse::SidecarEndpointUpdate` variant
- `core/acowork-core/src/proto_bridge.rs`: `sidecar_to_proto()` + two-way conversion of `SidecarEndpointUpdate`
- `core/acowork-runtime/src/grpc/client.rs`: `proto_to_gateway_response()` decodes `SidecarEndpointUpdate` → `GatewayResponse::SidecarEndpointUpdate`

**Retained** (only removed in C4):
- the `RuntimeConfigUpdate.embed_config_json` field
- the `GatewayResponse::EmbeddingConfigUpdate` variant
- the `GlobalResourcePusher::push_embedding_config()` function

**Additional (added while at it in C1)**:
- `GatewayRequest::UpdateConfig` gains the `builtin_tools_enabled_json` / `builtin_tools_all_json` fields (for two-way sync of ADR-029)
- `GatewayResponse::RuntimeConfigUpdate` gains the `builtin_tools_enabled: Option<Vec<String>>` field

### C2 (to do)

**Modified**:
- `core/acowork-gateway/src/ipc/global_push.rs`: add the generic `push_sidecar_endpoint(sidecar: SidecarKind, endpoint: String, spec_json: String)` method; mark `push_embedding_config()` with `#[deprecated(note = "use push_sidecar_endpoint(SidecarKind::Embed, ...) instead")]`, changing the body to call `push_sidecar_endpoint(SidecarKind::Embed, ...)` (kept as a thin shell)
- `core/acowork-gateway/src/lifecycle/embed_supervisor.rs`: migrate the 4 `pusher.push_embedding_config().await` call sites to `pusher.push_sidecar_endpoint(SidecarKind::Embed, endpoint, spec_json).await` (endpoint / spec_json are derived from `gw.embed_process`)
- `core/acowork-gateway/src/http/embedding_api.rs`: migrate the 1 `pusher.push_embedding_config().await` call site

**Untouched** (moved in C3):
- `core/acowork-gateway/src/lifecycle/lsp_relay_supervisor.rs` — during C2 the LSP relay supervisor is not wired to the pusher

### C3 (to do)

**Modified**:
- `core/acowork-runtime/src/tools/registry.rs`: `ToolRegistry.tools` changes from `Vec<Arc<dyn Tool>>` to `Arc<RwLock<Vec<Arc<dyn Tool>>>>`; add the `register_external()` / `unregister()` / `all_tools_snapshot()` APIs
- `core/acowork-runtime/src/agent/agent_core.rs` (or session_manager): add the `register_dynamic_tool(name, tool)` / `unregister_dynamic_tool(name)` business methods (registry change + rebuild_all_tools + broadcast to active sessions)
- `core/acowork-runtime/src/cli.rs`: in the main loop add the `SidecarEndpointUpdate` branch (route by `sidecar` kind: LspRelay → register/unregister the `codebase` tool; Embed → `embedding_manager.enable/disable_onnx_provider`)
- `core/acowork-gateway/src/lifecycle/lsp_relay_supervisor.rs`: add a `pusher: Option<Arc<GlobalResourcePusher>>` field to `LspRelaySupervisorConfig`; call `push_sidecar_endpoint(LspRelay, ...)` at the 5 state-change points
- `core/acowork-gateway/src/gateway/mod.rs`: pass the pusher into `start_lsp_relay_supervisor`
- `core/acowork-runtime/src/tools/builtin/mod.rs`: **retain** the startup-time registration logic of `all_builtin_tools()` via `lsp_relay_endpoint: Option<String>` (cooperating with the AgentHello snapshot); dynamic registration handles subsequent changes via `register_dynamic_tool` (same-name replacement, no duplication)
- `core/acowork-runtime/src/startup/agent_init.rs`: keep as-is — at startup, decide whether to register codebase based on `hello_config.lsp_relay_endpoint`

**Added / modified**:
- each agent package's `{work_dir}/config/agent_tools.json`: in the senior-engineer package, default `codebase.enabled = true` (relying on the sidecar push for registration)

**Untouched**:
- the Desktop App frontend (user at 06:20:38: "the frontend is not changed for now")

### C4 (to do)

**Cleanup**:
- `core/acowork-core/src/protocol.rs`:
  - remove the `embed_config_json: Option<String>` field from `RuntimeConfigUpdate`
  - remove the `EmbeddingConfigUpdate` variant from `GatewayResponse`
  - remove the `embed_config_json` field from `GatewayResponse::RuntimeConfigUpdate` (if present)
- `core/acowork-core/src/proto_bridge.rs`: delete the two-way conversion code for `embed_config_json`
- `core/acowork-runtime/src/grpc/client.rs`: delete the `RuntimeConfigUpdate.embed_config_json` receiving branch
- `core/acowork-runtime/src/cli.rs`: delete the `GatewayResponse::EmbeddingConfigUpdate` receiving branch
- `core/acowork-gateway/src/ipc/global_push.rs`:
  - delete the `push_embedding_config()` function
  - delete the population of the `embed_config_json` field at the `RuntimeConfigUpdate` construction sites
- `core/acowork-gateway/src/http/embedding_api.rs`: delete the `push_embedding_config()` call (already migrated to `push_sidecar_endpoint`; the call site can be simplified in sync)
- `core/acowork-gateway/src/lifecycle/embed_supervisor.rs`: delete the 4 `push_embedding_config()` calls (C2 has already migrated them to `push_sidecar_endpoint`; the comments can be simplified in sync)

**New constraint**:
- an older runtime (not upgraded to C1+) can still obtain `embed_endpoint` on its first AgentHello (the `AgentHelloResult` field is retained); but **while running**, embedding model switching/restarting no longer has a push channel
- this is a tradeoff the user accepted (06:22:13: "upgrade straight through; the project is still in development and there are no compatibility requirements")

---

## Background

### Status quo


Gateway currently manages two sidecar processes:
- **embed** (`acowork-embed`): the local ONNX embedding inference HTTP service, port 18080
- **lsp_relay** (`acowork-lsp-relay`): the LSP protocol JSON-RPC relay, port 19878

At startup, the Runtime obtains the initial endpoints via `GatewayResponse::AgentHelloResult` (lines 812-833 of `protocol.rs`):
- `embed_endpoint` / `embed_model_id` / `embed_dimension` — used to build the `FallbackEmbeddingProvider` chain
- `lsp_relay_endpoint` — used by `all_builtin_tools()` to decide whether to register the `codebase` tool

After startup, state changes of these two sidecars are pushed over two **different channels**:
- embed: the `GatewayResponse::RuntimeConfigUpdate.embed_config_json` field (`push_embedding_config()` at `global_push.rs:348-437`), in a JSON-inside-JSON shape
- lsp_relay: **no push channel at all**

### Problem 1: LSP relay state changes have no push channel

The state machine in `lsp_relay_supervisor.rs` is more complex than embed's:
- after startup, when the SSE connection succeeds it sets `lsp_relay_process.ready = true` (`lsp_relay_supervisor.rs:271-278`)
- heartbeat timeout / connection lost / restart / restart failure attaching to an existing process
- the reaper task, when it detects the child process exiting, clears `lsp_relay_process` (lines 172-176)

The Runtime is completely unaware of these state changes. If, when the Runtime starts, the LSP relay is not yet ready (the typical scenario: Gateway has just started, and the supervisor is still within its 30s startup grace period), the codebase tool is **not registered**, and it will not be auto-registered later when the LSP relay becomes ready.

### Problem 2: the codebase tool is registered once at startup, with no dynamic add/remove

Currently `all_builtin_tools()` decides whether to register the codebase tool once at startup, based on `lsp_relay_endpoint: Option<String>`:

```rust
// core/acowork-runtime/src/tools/builtin/mod.rs:140-145
// Only register codebase when the LSP Relay is available.
// Without the relay, the tool always fails with "LSP Relay not available",
// wasting LLM inference tokens on doomed calls.
if let Some(endpoint) = lsp_relay_endpoint {
    tools.push(Arc::new(codebase::CodebaseTool::new(endpoint)));
}
```

`ToolRegistry.tools: Vec<Arc<dyn Tool>>` is an immutable collection, with no `add_tool()` / `remove_tool()` methods. The Registry never changes once registered at `startup/agent_init.rs:353-365`.

### Problem 3: `embed_config_json` is JSON-inside-JSON

```rust
// the current push shape
embed_config_json: Some(serde_json::json!({
    "embed_endpoint": "http://127.0.0.1:18080/v1",
    "embed_model_id": "bge-small-zh-v1.5",
    "embed_dimension": 512,
}).to_string()),
```

The field names carry prefixes (`embed_endpoint` / `embed_model_id` / `embed_dimension`) because they must carry several related fields inside a single JSON. The Runtime side does another layer of `serde_json::from_str`. This mechanism is **not extensible** — adding another sidecar means adding another JSON field to `RuntimeConfigUpdate` and N separate fields to the proto.

### Problem 4: agent_tools.json lacks codebase

`{work_dir}/config/agent_tools.json` currently has 16 tools, missing `codebase`. Since registration of the codebase tool depends on the LSP relay being ready, if at startup the relay is not ready it is not registered, and there is **no way to recover** — this is exactly the problem this ADR addresses.

---

## Goals

1. **Generalize the sidecar push channel**: embed and lsp_relay share one message type `SidecarEndpointUpdate`
2. **Decouple the push semantics**: stop using the embedded JSON field inside `RuntimeConfigUpdate`
3. **Support dynamic registration/unregistration of builtin tools**: add a thread-safe `add_tool()` / `remove_tool()` interface to ToolRegistry
4. **Full sidecar lifecycle awareness**: the Runtime is aware of the entire state transition of sidecars from absent → ready → endpoint change → unavailable
5. **Zero cold-start latency**: AgentHello stays as it is (does not block waiting for sidecar readiness); subsequent sidecar readiness is backfilled by the push
6. **Transition-period compatibility**: during C2/C3 the `embed_config_json` field is retained and cleaned up in one shot in C4 (the user confirmed no old-runtime compatibility is needed)

---

## Detailed Design

### Phase C1: the protocol layer (✅ done)

> **Goal**: establish a generic sidecar push channel on the wire protocol, **removing no old field**
> **Current state**: the protocol fields, the proto, and the client.rs decoding are all in place (HEAD)

#### C1.1 the new `SidecarKind` enum

```rust
// core/acowork-core/src/protocol.rs
/// Identifies a Gateway-managed sidecar process. The Runtime uses this to
/// route a `SidecarEndpointUpdate` to the correct subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarKind {
    /// Reserved for forward-compat. Treated as "unknown" by the Runtime.
    Unspecified,
    /// acowork-lsp-relay — provides JSON-RPC LSP relay used by the
    /// Runtime's `codebase` builtin tool.
    LspRelay,
    /// acowork-embed — local ONNX embedding HTTP service. The Runtime
    /// builds a `FallbackEmbeddingProvider` chain from the active model
    /// id and dimension provided in the push payload.
    Embed,
}

impl SidecarKind {
    pub fn as_str(&self) -> &'static str { ... }
}
impl std::str::FromStr for SidecarKind { ... }
```

- `as_str()` / `FromStr` provide a stable wire string representation
- adding a new sidecar only requires appending an enum variant + a proto value (never rename / reorder)

#### C1.2 the new `GatewayResponse::SidecarEndpointUpdate` message

```rust
/// Sidecar endpoint update (Gateway → Runtime, push).
SidecarEndpointUpdate {
    /// Which sidecar this update is for.
    sidecar: SidecarKind,
    /// HTTP URL the Runtime should use. Empty string = sidecar unavailable.
    endpoint: String,
    /// Sidecar-specific metadata. Schema depends on `sidecar`:
    ///   - LspRelay: "" (no extra fields today)
    ///   - Embed:    {"model_id":"bge-small-zh-v1.5","dimension":512}
    /// Empty string if no metadata applies.
    spec_json: String,
},
```

**Key decision**: **an empty string = the sidecar is unavailable** (rather than using `Option<String>` + a None field). A proto field cannot express the "present/absent" ambiguity of `Option<String>`; an empty string is the natural "none" marker.

#### C1.3 the proto binding

```protobuf
// core/acowork-core/proto/gateway_ipc.proto
enum SidecarKind {
    SIDECAR_KIND_UNSPECIFIED = 0;
    SIDECAR_KIND_LSP_RELAY = 1;
    SIDECAR_KIND_EMBED = 2;
}

message SidecarEndpointUpdate {
    SidecarKind sidecar = 1;
    string endpoint = 2;       // empty = unavailable
    string spec_json = 3;      // empty = no extra fields
}

// add tag 44 to ServerMessage.payload
```

`proto_bridge.rs` provides the `sidecar_to_proto()` / `sidecar_from_proto()` conversion functions + two-way conversion of `SidecarEndpointUpdate`.

#### C1.4 gRPC client decoding

```rust
// core/acowork-runtime/src/grpc/client.rs:1310-1331
Some(ServerPayload::SidecarEndpointUpdate(seu)) => {
    let sidecar = match seu.sidecar {
        x if x == proto::SidecarKind::LspRelay as i32 => SidecarKind::LspRelay,
        x if x == proto::SidecarKind::Embed as i32 => SidecarKind::Embed,
        _ => SidecarKind::Unspecified,
    };
    tracing::info!(sidecar = %sidecar.as_str(), endpoint = %seu.endpoint, ...);
    GatewayResponse::SidecarEndpointUpdate { sidecar, endpoint: seu.endpoint, spec_json: seu.spec_json }
}
```

**C1 boundary**: after the client finishes decoding it only logs; it does **not** actually route into AgentCore — that is C3's job.

#### C1 acceptance

- ✅ `cargo build --workspace` passes
- ✅ `cargo test --workspace` passes
- ✅ the `SidecarKind` unit test covers wire string stability
- ✅ the proto ↔ domain two-way conversion unit test
- ✅ older runtimes still work (the `embed_config_json` field is not removed)

---

### Phase C2: the Gateway push layer (to do)

> **Goal**: implement `push_sidecar_endpoint()` as the generic push channel, and migrate the embed supervisor to it
> **The LSP relay supervisor is untouched** (only wired in C3)
> **The runtime is untouched**

#### C2.1 the new `GlobalResourcePusher::push_sidecar_endpoint()` method

```rust
// core/acowork-gateway/src/ipc/global_push.rs
/// Push a sidecar endpoint update to all running agents.
/// This is the canonical channel for sidecar state changes
/// (lsp_relay ready, embed model switched, sidecar crash, ...).
/// Empty `endpoint` signals "sidecar is unavailable" — the Runtime
/// should disable dependent features rather than try to connect.
#[tracing::instrument(skip(self), name = "push_sidecar_endpoint")]
pub async fn push_sidecar_endpoint(
    &self,
    sidecar: SidecarKind,
    endpoint: String,
    spec_json: String,
) {
    let grpc_session_mgr = match &self.grpc_session_mgr {
        Some(mgr) => mgr.clone(),
        None => {
            tracing::warn!(sidecar = %sidecar.as_str(), "No gRPC session manager, skipping sidecar push");
            return;
        }
    };

    let agent_ids: Vec<String> = {
        let gw = self.gateway_state.read().await;
        gw.running_agents.keys().cloned().collect()
    };

    if agent_ids.is_empty() {
        return;
    }

    let mut pushed = 0u32;
    let mut failed = 0u32;
    for agent_id in agent_ids {
        let mgr = grpc_session_mgr.lock().await;
        if let Some((_conn_id, session)) = mgr.find_by_agent_id(&agent_id) {
            let ok = session.push_message(GatewayResponse::SidecarEndpointUpdate {
                sidecar,
                endpoint: endpoint.clone(),
                spec_json: spec_json.clone(),
            }).await;

            if ok {
                tracing::info!(agent = %agent_id, sidecar = %sidecar.as_str(), "Pushed sidecar endpoint to agent");
                pushed += 1;
            } else {
                tracing::warn!(agent = %agent_id, sidecar = %sidecar.as_str(), "Sidecar push failed (channel closed)");
                failed += 1;
            }
        }
    }

    if pushed > 0 || failed > 0 {
        tracing::info!(sidecar = %sidecar.as_str(), pushed, failed, "Sidecar push complete");
    }
}
```

#### C2.2 mark `push_embedding_config()` as deprecated

```rust
/// DEPRECATED: Use `push_sidecar_endpoint(SidecarKind::Embed, ...)` instead.
/// This method now delegates to the generic sidecar channel but is
/// retained for backward compatibility with external callers (tests,
/// ad-hoc scripts). It will be removed in C4.
#[deprecated(note = "use push_sidecar_endpoint(SidecarKind::Embed, ...) instead")]
#[tracing::instrument(skip(self), name = "push_embedding_config")]
pub async fn push_embedding_config(&self) {
    // ... existing embed_endpoint extraction logic ...
    self.push_sidecar_endpoint(SidecarKind::Embed, endpoint, spec_json).await;
}
```

This keeps external test code working, with a compile-time deprecation warning nudging the migration.

#### C2.3 migrate the embed supervisor call sites

`lifecycle/embed_supervisor.rs` has 4 existing `pusher.push_embedding_config().await` call sites (lines 321, 337, 600, 709):

```rust
// before migration
if let Some(p) = &pusher {
    p.push_embedding_config().await;
}

// after migration
if let Some(p) = &pusher {
    p.push_sidecar_endpoint(SidecarKind::Embed, endpoint, spec_json).await;
}
```

Inside `push_embedding_config()` there is already logic for extracting (endpoint, model_id, dimension) from `gw.embed_process` (global_push.rs:367-382); that extraction needs to be factored out into a **standalone helper function** `build_embed_sidecar_payload()` so both `push_embedding_config()` and the embed supervisor can call it, avoiding duplication:

```rust
// global_push.rs
fn build_embed_sidecar_payload(state: &GatewayState) -> Option<(String, String)> {
    let eps = state.embed_process.as_ref()?;
    if eps.active_model_id.is_none() {
        return None;
    }
    let endpoint = format!("http://127.0.0.1:{}/v1", eps.port);
    let spec_json = serde_json::json!({
        "model_id": eps.active_model_id.clone().unwrap_or_default(),
        "dimension": eps.active_dimension.unwrap_or(0),
    }).to_string();
    Some((endpoint, spec_json))
}
```

The embed supervisor becomes:

```rust
if let Some((endpoint, spec_json)) = build_embed_sidecar_payload(&state) {
    pusher.push_sidecar_endpoint(SidecarKind::Embed, endpoint, spec_json).await;
}
```

The codebase tool is still registered only once at startup based on `AgentHelloConfig.lsp_relay_endpoint` (if the LSP relay was ready at that moment).

C3 is what wires the LSP relay supervisor to the pusher.

#### C2 acceptance

- `cargo build --workspace` passes
- `cargo test --workspace` passes (deprecation warnings are visible but do not break compilation)
- `global_push.rs` unit tests cover the `push_sidecar_endpoint()` call
- existing `embed_supervisor` unit tests do not break
- existing `lsp_relay_supervisor` unit tests do not break (C2 does not touch it)
- End-to-end: an embed model switch → `push_sidecar_endpoint(Embed, ...)` push → an older runtime (still reading `embed_config_json`) **also** receives the embed config (via the `push_embedding_config()` deprecated wrapper compatibility)
- End-to-end: LSP relay ready / restart → no push at present (behaviour unchanged, wired in C3)

---

### Phase C3: the Runtime business layer + LSP relay supervisor wiring (to do)

> **Goal**: make the Runtime actually respond to `SidecarEndpointUpdate` messages, dynamically register/unregister the `codebase` tool, and trigger rebuild of the embed provider chain
> **Also completes**: wiring the LSP relay supervisor to the pusher (previously always missing)
> **The frontend is untouched**

#### C3.1 add dynamic add/remove APIs to `ToolRegistry`

Current `tools/registry.rs:18-21`:

```rust
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}
```

Refactor to:

```rust
pub struct ToolRegistry {
    /// Internal mutable collection, protected by RwLock for thread safety.
    /// The legacy `Vec<Arc<dyn Tool>>` is replaced with `Arc<RwLock<Vec<...>>>`
    /// so `add_tool` / `remove_tool` can be called concurrently with
    /// `all_tools` / `tool_names` / `activate`.
    tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Arc::new(RwLock::new(Vec::new())) }
    }

    /// Register a tool. If a tool with the same name exists, it is replaced.
    /// No-op if the same instance is already registered.
    pub async fn register_external(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().await;
        if let Some(existing) = tools.iter().position(|t| t.name() == name) {
            tools[existing] = tool;
            tracing::info!(tool = %name, "Replaced existing tool in registry");
        } else {
            tools.push(tool);
            tracing::info!(tool = %name, "Added tool to registry");
        }
    }

    /// Remove a tool by name. Returns true if found and removed.
    pub async fn unregister(&self, name: &str) -> bool {
        let mut tools = self.tools.write().await;
        let before = tools.len();
        tools.retain(|t| t.name() != name);
        let removed = tools.len() < before;
        if removed {
            tracing::info!(tool = %name, "Removed tool from registry");
        }
        removed
    }

    /// Async snapshot of the current tool list.
    pub async fn all_tools_snapshot(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.read().await.clone()
    }

    /// Synchronous accessor — returns a snapshot via try_read.
    /// None if the lock is held (callers should fall back to async).
    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.try_read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    // ... refactor the existing register() / tool_names() / activate() ...
}
```

**API compatibility constraint**:
- keep the signatures of the existing `register()` / `all()` / `tool_names()` / `activate()` as much as possible; use try_read / try_write internally as a fallback
- add the new `register_external()` / `unregister()` / `all_tools_snapshot()` async APIs

#### C3.2 add `register_dynamic_tool` / `unregister_dynamic_tool` to `AgentCore`

Expose the business methods in `agent/agent_core.rs` or `session/session_manager.rs`:

```rust
impl AgentCore {
    /// Register a tool dynamically. Used by the SidecarEndpointUpdate handler
    /// when lsp_relay becomes available. Also called by MCP hot-add flows.
    pub async fn register_dynamic_tool(&self, name: &str, tool: Arc<dyn Tool>) -> Result<()> {
        // 1. register into the ToolRegistry
        self.tool_registry.register_external(tool).await;
        // 2. rebuild all_tools (merge builtin + dynamic)
        self.rebuild_all_tools().await?;
        // 3. broadcast to all active sessions (so the running LLM loop sees the new tool)
        self.broadcast_tool_change().await;
        Ok(())
    }

    /// Unregister a dynamic tool. Used when lsp_relay becomes unavailable.
    pub async fn unregister_dynamic_tool(&self, name: &str) -> Result<()> {
        if !self.tool_registry.unregister(name).await {
            return Ok(());  // the tool was not registered at all — idempotent
        }
        self.rebuild_all_tools().await?;
        self.broadcast_tool_change().await;
        Ok(())
    }
}
```

#### C3.3 route `SidecarEndpointUpdate` in `cli.rs`

Add a `SidecarEndpointUpdate` branch to the main loop in `cli.rs` (next to the `RuntimeConfigUpdate` branch):

```rust
GatewayResponse::SidecarEndpointUpdate { sidecar, endpoint, spec_json } => {
    match sidecar {
        SidecarKind::LspRelay => {
            if endpoint.is_empty() {
                agent_core.unregister_dynamic_tool("codebase").await?;
                tracing::info!("LSP Relay unavailable: removed codebase tool");
            } else {
                let tool: Arc<dyn Tool> = Arc::new(
                    crate::tools::builtin::codebase::CodebaseTool::new(endpoint.clone())
                );
                agent_core.register_dynamic_tool("codebase", tool).await?;
                tracing::info!(endpoint = %endpoint, "LSP Relay available: registered codebase tool");
            }
        }
        SidecarKind::Embed => {
            if endpoint.is_empty() {
                // embed unavailable: remove the ONNX provider
                embedding_manager.disable_onnx_provider().await?;
            } else {
                let spec: EmbedSidecarSpec = serde_json::from_str(&spec_json)
                    .map_err(|e| format!("invalid embed spec_json: {e}"))?;
                embedding_manager.enable_onnx_provider(
                    endpoint.clone(),
                    spec.model_id,
                    spec.dimension,
                ).await?;
            }
        }
        SidecarKind::Unspecified => {
            tracing::warn!("Received SidecarEndpointUpdate with Unspecified kind; ignoring");
        }
    }
    LoopAction::Continue
}
```

The `EmbedSidecarSpec` struct:

```rust
#[derive(Debug, Deserialize)]
struct EmbedSidecarSpec {
    model_id: String,
    dimension: usize,
}
```

#### C3.4 wire the LSP relay supervisor to the pusher

Add a `pusher: Option<Arc<GlobalResourcePusher>>` field to `LspRelaySupervisorConfig`, add a pusher parameter to the `start_lsp_relay_supervisor()` signature, and internally in run_supervisor call `push_sidecar_endpoint(LspRelay, ...)` at 5 state-change points:

| Location | Event | Push content |
|------|------|---------|
| `lsp_relay_supervisor.rs:271-278` | SSE connection succeeded, mark ready | endpoint = `http://127.0.0.1:{port}`, spec = "" |
| `lsp_relay_supervisor.rs:137-139` | clear lsp_relay_process before restart | endpoint = "", spec = "" |
| `lsp_relay_supervisor.rs:145-147` | give up on exceeding the restart limit | endpoint = "", spec = "" |
| `lsp_relay_supervisor.rs:162-165` | restart succeeded (new PID) | endpoint = `http://127.0.0.1:{port}`, spec = "" |
| `lsp_relay_supervisor.rs:172-176` | the reaper detects the child process exiting | endpoint = "", spec = "" |

Like embed, factor out a helper:

```rust
fn build_lsp_relay_sidecar_payload(state: &GatewayState, default_port: u16) -> (String, String) {
    let endpoint = state.lsp_relay_process.as_ref()
        .filter(|p| p.ready)
        .map(|p| format!("http://127.0.0.1:{}", p.port))
        .unwrap_or_default();
    (endpoint, String::new())  // spec_json is always empty
}
```

`gateway/mod.rs` passes the pusher when calling `start_lsp_relay_supervisor`.

#### C3.5 keep the startup-time codebase registration path as-is

`startup/agent_init.rs:343-362` currently reads the LSP relay endpoint from `AgentHelloConfig.lsp_relay_endpoint` to decide whether to register codebase. **C3 does not change this**:

- if the LSP relay is ready at startup → register codebase
- if the LSP relay is not ready at startup → do not register; wait for the SidecarEndpointUpdate push
- `register_external()` detects a same-name and **replaces** it (no duplicate registration)

#### C3.6 enable codebase by default in `agent_tools.json`

`{work_dir}/config/agent_tools.json` currently has 16 tools, missing codebase. C3 adds it to the senior-engineer package:

```json
{
  "name": "codebase",
  "enabled": true
}
```

**Note**: `enabled = true` is the **expected state** — whether it is actually registered depends on whether the LSP relay is ready. If the LSP relay has not come up, `register_external()` is not called, so there is no codebase in the registry, and the `codebase` in `enabled_entries` is **silently skipped** (the `registry.rs:55` comment: "Tools NOT in the registry but listed in `enabled_entries` are silently skipped").

This is exactly what we want: LSP relay not up → the tool panel does not show codebase (avoiding "doomed calls"); LSP relay up → the push triggers register_external → codebase appears in the tool panel.

#### C3 acceptance

- `cargo build --workspace` passes
- `cargo test --workspace` passes
- `ToolRegistry::register_external/unregister` unit tests (including same-name replacement, an empty registry, and concurrency)
- `AgentCore::register_dynamic_tool` unit tests (rebuild + broadcast)
- `cli.rs` `SidecarEndpointUpdate` branch unit tests covering the LspRelay / Embed / Unspecified kinds
- **End-to-end**:
  1. start Gateway (LSP relay not yet ready)
  2. start the senior-engineer agent → the tool panel does **not** show codebase
  3. wait for the LSP relay supervisor to mark ready → the agent receives SidecarEndpointUpdate → the tool panel **shows** codebase
  4. kill the LSP relay process → the reaper pushes endpoint="" → the tool panel **removes** codebase
  5. switch the embed model → push SidecarEndpointUpdate(Embed, new_endpoint, new_spec) → the agent rebuilds the ONNX provider

---

### Phase C4: protocol cleanup (to do)

> **Goal**: fully remove the old `embed_config_json` field and the `EmbeddingConfigUpdate` variant
> **User confirmed**: "upgrade straight through; the project is still in development and there are no compatibility requirements" (06:22:13)

#### C4.1 remove the `RuntimeConfigUpdate.embed_config_json` field

Delete the field definition at `core/acowork-core/src/protocol.rs:1080` + all construction sites.

Construction-site checklist (to be confirmed by search at C4 time):
- `push_mcp_catalog()` (line 250-262) in `core/acowork-gateway/src/ipc/global_push.rs`
- any other occurrence of `RuntimeConfigUpdate { ..., embed_config_json: Some(...), ... }`

#### C4.2 remove the `GatewayResponse::EmbeddingConfigUpdate` variant

Delete the entire variant at `core/acowork-core/src/protocol.rs:1134-1146` + the corresponding conversion code in `proto_bridge.rs` + the decoding branch in `grpc/client.rs` + the handling branch in `cli.rs`.

#### C4.3 remove the `push_embedding_config()` function

Delete the function at `core/acowork-gateway/src/ipc/global_push.rs:348-437` + the `build_embed_sidecar_payload()` helper (already moved inside `push_sidecar_endpoint`).

#### C4.4 clean up call sites

- the 4 `push_embedding_config()` calls in `core/acowork-gateway/src/lifecycle/embed_supervisor.rs` — **already** migrated to `push_sidecar_endpoint` in C2; cleaned up in sync in C4
- the 1 call in `core/acowork-gateway/src/http/embedding_api.rs` — same as above
- any external test code calling `push_embedding_config()` must be migrated or deleted in sync

#### C4.5 protocol doc updates

- mark C4 complete in `docs/adr/zh/ADR-030-...`
- update `docs/design/zh/12-tool-system.md` and any other docs that reference the old fields

#### C4 acceptance

- ✅ `cargo build --workspace` passes (no deprecation warnings)
- ✅ `cargo test --workspace` passes
- ✅ searching for `embed_config_json` in the codebase yields 0 hits (excluding historical narrative in ADR docs)
- ✅ searching for `EmbeddingConfigUpdate` in the codebase yields 0 hits (excluding historical narrative in ADR docs)
- ✅ end-to-end: an embed model switch → `push_sidecar_endpoint(Embed, ...)` push → after the runtime receives it, it rebuilds the provider chain

#### C4 compatibility impact

- **first AgentHello**: the Runtime can still get the initial endpoint from `AgentHelloResult.embed_endpoint` (that field is not touched)
- **running embed model switch**: an older runtime (a pre-C4 version) **does not receive the push** — but the first startup still works
- this is a tradeoff the user accepted (the project is still in development, no compatibility requirement)

---

## Key Design Decisions

### D1: why use a standalone message `SidecarEndpointUpdate` rather than an embedded field in `RuntimeConfigUpdate`?

| Dimension | Embedded field (old) | Standalone message (new) |
|------|---------------|---------------|
| proto expressiveness | every new sidecar adds N separate fields | add one enum variant + reuse spec_json |
| type safety | JSON-in-JSON field names are easy to misspell | the proto enum enforces the type |
| semantic clarity | `RuntimeConfigUpdate` mixes LLM params + embed endpoints | single responsibility |
| extensibility | hard to add a new sidecar | just add an enum variant |
| cost | — | one more proto message |

**Choose the standalone message**. The old `embed_config_json` field is retained during the C1/C2/C3 transition and cleaned up in one shot in C4.

### D2: why does `endpoint = ""` mean "unavailable" rather than `Option<String>`?

A proto field cannot express the None semantics of `Option<String>`. An empty string is the natural "none" marker, and the Runtime just checks `endpoint.is_empty()`. spec_json likewise uses an empty string to mean "no metadata".

### D3: does changing ToolRegistry to `Arc<RwLock<Vec<...>>>` hurt performance?

`add_tool` / `remove_tool` are low-frequency operations (sidecar state changes at most a few times a minute), so an `RwLock` is fine. `all()` becomes a `try_read` snapshot; in the vast majority of cases the lock is idle and a consistent view is obtained. The `all_tools_snapshot()` async API is for the scenarios that must guarantee consistency.

### D4: keep the startup-time codebase registration at startup?

**Keep it.** Reasons:
- minimal change; C3's `register_external` naturally covers post-startup changes
- AgentHelloConfig already carries the lsp_relay_endpoint field (line 833)
- even if the LSP relay crashes immediately after startup, the supervisor pushes `endpoint = ""` to trigger `unregister`
- startup-time + dynamic registration are **complementary, not conflicting**: the same-name replacement semantics of `register_external`

### D5: why not deprecate `AgentHelloConfig.lsp_relay_endpoint`?

AgentHelloResult is the configuration snapshot **at handshake time**; SidecarEndpointUpdate is the push update **after handshake**. They **do not conflict**:
- at startup the Runtime uses AgentHelloConfig to decide the initial state
- while running the Runtime uses SidecarEndpointUpdate to respond to changes

The extension of `SidecarKind` does not affect AgentHelloConfig either — it only carries the single LSP relay endpoint field; in the future, adding a new sidecar only requires adding an enum variant to `SidecarEndpointUpdate`.

### D6: the L1 Readiness Barrier was rejected — why?

The user explicitly rejected it at 05:23:36:
> "embed/lsp are process-startup initializations, the delay is on the order of seconds, and more subprocesses may be introduced later; waiting for all processes to be ready makes the delay unacceptable"

The correct approach: AgentHello stays as it is (the snapshot returns immediately, no blocking), and subsequent sidecar readiness is backfilled by the pusher. L1 looks like it "treats the root cause" but actually introduces a new global startup delay that grows every time a sidecar is added.

### D7: after embed moves to SidecarEndpointUpdate, how does old-runtime compatibility look?

- during C2/C3: the `push_embedding_config()` deprecated wrapper lets an older runtime (still reading `embed_config_json`) **also** receive the push — two channels coexist
- after C4: an older runtime loses the running embed config push, but the first AgentHello still works
- this is a tradeoff the user accepted (06:22:13: "upgrade straight through; the project is still in development, no compatibility requirement")

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| duplicate registration of codebase between the LSP relay startup period and push period | flicker in the tool panel | same-name replacement via `register_external`; on the first push, skip if already registered |
| after C4 removes the `embed_config_json` field, older runtimes fail silently | users who have not finished upgrading do not notice the embed restart | document the upgrade path clearly; the runtime binary is released separately, not tightly coupled to the gateway |
| the LSP relay supervisor has many state-change points and a push is easily missed | some edge cases are not pushed | at C3 acceptance, run the three scenarios: supervisor restart / kill -9 / port conflict |
| the embed supervisor, after migrating to push_sidecar_endpoint, has changed semantics | the Runtime receives duplicate pushes | dedup by name on the Runtime-side `register_external`; the embed provider chain detects changes by endpoint |

---

## Decision Record

- 2026-07-08: draft created. Phase C1 (the protocol layer) is done (HEAD); C2/C3/C4 to implement.
- 2026-07-08: the user confirmed Path B (C1→C2→C3→C4, 4 independent buildable commits).
- 2026-07-08: the user confirmed fully migrating embed to SidecarEndpointUpdate (option B; the old channel is cleaned up in C4).
- 2026-07-08: the user confirmed the frontend is out of scope for these 4 commits.
- 2026-07-08: the user rejected the L1 Readiness Barrier, confirming AgentHello stays as-is + the pusher backfills asynchronously.
