# ADR-031: Dropping the Legacy IPC Channel Remnants — Full Consolidation onto gRPC

> **Chinese source of truth**: [ADR-031](../zh/ADR-031-drop-legacy-ipc-consolidate-on-grpc.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented

## Date

2026-07-09

## Decision Makers

大鱼 (Dayu)

## Predecessor

[ADR-016](../zh/ADR-016-centralized-exception-handling.md) (IPC → gRPC migration design)

---

## Decision Summary

**6 atomic commits, each independently buildable and fully testable**:

| Commit | Scope | Files | LOC | Risk |
|--------|-------|-------|-----|------|
| **C1** | Gateway `ipc/server.rs` → `handlers/` (pure module rename) | ~16 | +120 / -120 | low |
| **C2** | merge `SessionManager` + `GrpcSessionManager` into a single registry | ~8 | +250 / -400 | medium |
| **C3** | Gateway `ipc/global_push.rs` → `grpc/resource_pusher.rs` | ~8 | +60 / -60 | low |
| **C4** | Runtime deletes the entire `pub mod ipc` (an empty shell) | ~3 | +5 / -45 | low |
| **C5** | delete the `socket_path` config / rename `gateway_socket` | ~8 | +30 / -50 | low |
| **C6** | proto package `acowork.ipc.v1` → `acowork.gateway.v1` + comment cleanup | ~4 | +20 / -15 | medium (breaking) |

**Key decisions**:

| Decision | Rationale |
|----------|-----------|
| No deprecation window, delete outright | the project is pre-release with no external consumers (ADR-016 §1.3 already established this principle) |
| Merge `SessionManager` into `GrpcSessionManager`, no new type introduced | fewer types; `GrpcSession` is already feature-complete |
| Delete the dead `Session` fields `pending_requests` / `next_id` | 0 references in production code (test-only) |
| Rename the proto package in one shot, no dual package name | eradicate the `ipc` naming residue; `acowork.gateway.v1` is semantically accurate |
| Keep CLI `--gateway-socket` as a deprecated alias | avoid breaking existing Runtime CLI callers (e.g. system scripts) |

---

## Context

The gRPC transport migration (ADR-016) is 100% complete:

- The Gateway **only listens** on a tonic gRPC server at TCP `127.0.0.1:19877`
- The Runtime **only dials** via `GatewayGrpcClient::connect()` to that gRPC endpoint
- No `TcpListener` / `UnixListener` / `UnixStream` handling the old 5-byte fixed header + JSON body protocol remains

What remains is **misleading module naming** and **architectural redundancy**:

### Residue 1: the misleading `ipc/server.rs` name — 1385 lines

`core/acowork-gateway/src/ipc/server.rs` contains **no server code whatsoever** (no bind, no
accept, no connect, no frame read/write). It is a pure collection of business-logic functions:

```rust
handle_key_release()      // 14 handler functions in total
handle_intent_send()      // all referenced only by grpc/dispatch.rs
handle_budget_query()
handle_usage_report()
handle_rate_acquire()
handle_capability_query()
handle_cron_register()
handle_cron_unregister()
handle_cron_list()
handle_context_usage_report()
handle_agent_hello()
handle_agent_ready()
// + ResolvedLlmConfig / resolve_llm_config_for_agent()
```

The doc comment is stale:

```rust
//! handlers are shared between the gRPC server (grpc/dispatch.rs)
//! and can be used by any transport layer.    // ← there is no "any transport" any more
```

### Residue 2: dual SessionManager — ~150 lines of architectural redundancy

`core/acowork-gateway/src/grpc/server.rs:454-464`:

```rust
pub struct GatewayGrpcService {
    grpc_session_mgr: SharedGrpcSessionMgr,  // ✅ gRPC-only, GrpcSession
    ipc_session_mgr: SharedSessionMgr,       // ❌ legacy Session, double-registered per connection
}
```

Every gRPC connection **registers into both managers** (`server.rs:488-501`):

```rust
mgr.create_session(&conn_id, outbound_tx.clone());
// and simultaneously into the legacy SessionManager (for handler compatibility)
mgr.create_session_with_push(&conn_id, ipc_push_tx);
```

The only reason: dispatch handlers look up `SessionManager` by `conn_id` to get `agent_id` — and
`GrpcSessionManager` has exactly the same lookup capability.

`Session`'s `pending_requests` / `next_id` / `push_tx` / `push_message` fields have **0 references in
production code** (only in their own tests).

### Residue 3: `ipc/global_push.rs` in the wrong place — 475 lines

`GlobalResourcePusher` pushes **only through `SharedGrpcSessionMgr`**:

```rust
use crate::grpc::SharedGrpcSessionMgr;     // ← depends on the gRPC module
pub struct GlobalResourcePusher {
    grpc_session_mgr: Option<SharedGrpcSessionMgr>,  // ← the field is gRPC too
```

Its location under `ipc/` is purely historical.

### Residue 4: Runtime `pub mod ipc` empty shell — 37 lines

`runtime/src/ipc/client.rs` defines `LlmConfigReceived`, already fully superseded by
`AgentHelloConfig` in the gRPC client and 0-referenced inside the Runtime — yet `lib.rs:15` still
exposes `pub mod ipc;`.

### Residue 5: CLI/config misleading names

| Location | Field name | Actual semantics | Problem |
|----------|-----------|------------------|---------|
| `gateway/config.rs:61` | `socket_path` | **unused** | defaults to `gateway.sock`, but nothing binds/removes the file |
| `gateway/cli.rs:54` | `--socket-path` | same | passed around but never consumed |
| `runtime/config.rs:52` | `gateway_socket` | gRPC URL | misleading name |
| `runtime/cli.rs:56-57` | `--gateway-socket` / `ACOWORK_GATEWAY_SOCKET` | gRPC URL | misleading name |
| `runtime/startup/context.rs:82` | `socket_path` | gRPC URL | misleading, and discarded as `_socket_path` right after `run_gateway_loop` |

### Residue 6: proto package name `acowork.ipc.v1` + comments

- the proto package `acowork.ipc.v1` → renamed to `acowork.gateway.v1`
- ~20 stale "IPC" mentions cleaned across the workspace (both Gateway and Runtime)
- log strings such as `"IPC session manager"` → `"gRPC session manager"`

---

## Implementation Plan

### Commit C1: Gateway `ipc/server.rs` → `handlers/`

1. `git mv core/acowork-gateway/src/ipc/server.rs` → `core/acowork-gateway/src/handlers/server.rs`
2. `git mv core/acowork-gateway/src/ipc/session.rs` → `core/acowork-gateway/src/handlers/session_state.rs`
3. new `core/acowork-gateway/src/handlers/mod.rs` re-exporting `session_state::{Session, SessionManager}`
4. `gateway/lib.rs:16`: `pub mod ipc;` → `pub mod handlers;`
5. update all `use crate::ipc::server::{...}` → `use crate::handlers::{...}`

~16 files touched. **C1's key rule: do not substantially change logic, only fix import paths.**
C2 changes the architecture.

### Commit C2: merge the dual SessionManager (the core work)

Delete `ipc_session_mgr: SharedSessionMgr` and its double registration; all handlers use
`GrpcSessionManager` directly.

**Analysis**: the only production-active fields of `ipc::Session` are `agent_id` and
`connection_role` — both also present on `GrpcSession`. `GrpcSession` additionally has `push_tx` (the
gRPC outbound), the proxy push methods `push_message` / `push_proto` / `push_request`, and the
`pending_requests` / `session_requests` / `next_request_id` HTTP→Runtime request-response plumbing.
The legacy `Session::push_message(GatewayResponse)` is already covered by `GrpcSession::push_message`
(which internally calls `to_proto()` to forward to the gRPC outbound).

**Steps**:

1. **Change handler signatures** — `handle_key_release(provider, conn_id, state, session_mgr)` →
   `handle_key_release(provider, conn_id, state)`; resolve `agent_id` via
   `grpc_session_mgr.get_session(conn_id)`, or preferably have `dispatch.rs` extract `agent_id`
   up-front and pass it in, so handlers become pure business-logic functions fully decoupled from
   the connection layer. Applied to all 14 handlers.
2. **Change `dispatch_grpc_request`'s signature** — `session_mgr` → `agent_id`.
3. **`GatewayGrpcService::connect` drops the double registration** — delete
   `ipc_session_mgr.lock().await.create_session_with_push(...)` and the matching
   `remove_session(&conn_id_clone)`; keep only `grpc_session_mgr.create_session(...)`.
4. **Delete the `ipc_session_mgr` field** from `GatewayGrpcService`.
5. **Remove Branch 2 of the `tokio::select!`** (`ipc_push_rx.recv()` bridging) — `GrpcSession::push_message`
   already sends directly to `self.push_tx` with the `to_proto(0)` conversion, so Branch 2 is a
   redundant path.
6. **Delete the dead `Session` fields**: `pending_requests`, `next_id`, `push_tx: Option<PushSender>`.
7. **Update all references**: `grpc/server.rs`, `cron/mod.rs` (`find_by_agent_id` → grpc manager),
   `intent/router.rs`, `http/routes.rs` (delete the `AppState.session_mgr` field), `gateway/mod.rs`.

**Risks**:

| Risk | Level | Handling |
|------|-------|----------|
| cron handler looks up session by `conn_id` | 🟡 | change the cron handler to take an `agent_id` parameter |
| `intent/router.rs` async/sync route depends on `session_mgr` | 🟡 | change to take `&SharedGrpcSessionMgr` |
| push path breaks after Branch 2 removal | 🔴 high | audit every push caller (checklist below), each with a `tracing!` debug line |
| `dispatch.rs`'s `get_session(conn_id)` for `agent_id` | 🟡 | `GrpcSession` provides equivalent capability |

**Push audit checklist** — every `GatewayResponse` pushed to a Runtime must go through
`GrpcSessionManager::push_to_agent()` and not the legacy `Session::push_message()`:

| Push source | Current path | Verdict |
|-------------|--------------|---------|
| `http/agents.rs:742, 1010, 2030, 2457` | `push_message(GatewayResponse::RuntimeConfigUpdate)` | ✅ already via `SharedGrpcSessionMgr` |
| `http/embedding_api.rs` | `build_embed_sidecar_payload` → push | ✅ |
| `lifecycle/embed_supervisor.rs` | `Pusher::push_sidecar_endpoint()` | ✅ |
| `lifecycle/lsp_relay_supervisor.rs` | same | ✅ |
| `lifecycle/manager.rs:190` | `push_message(GatewayResponse::EnableDebugMode)` | ✅ |
| `intent/router.rs:174` | `push_message(GatewayResponse::IntentReceived)` | to confirm |
| `http/question.rs:75` | `push_message(GatewayResponse::IntentReceived)` | to confirm |

### Commit C3: `global_push.rs` → `grpc/resource_pusher.rs`

`git mv` the file, add `pub mod resource_pusher;` + `pub use resource_pusher::GlobalResourcePusher;`
to `grpc/mod.rs`, update 6 `use crate::ipc::global_push::*` sites, and (with `pub mod ipc` now gone)
delete the entire `gateway/src/ipc/` directory.

### Commit C4: Runtime deletes `pub mod ipc`

`rm -rf core/acowork-runtime/src/ipc/`, remove `pub mod ipc;` from `lib.rs:15`, verify with
`cargo build`.

### Commit C5: config/CLI renames

1. delete `GatewayConfig::socket_path` (definition, the `default()` computation, the `from_cli()`
   assignment, and the `assert!(!config.socket_path.is_empty())` test assertion)
2. delete Gateway CLI `--socket-path` / `ACOWORK_GATEWAY_SOCKET_PATH`
3. `RuntimeConfig::gateway_socket` → `RuntimeConfig::gateway_endpoint`
4. Runtime CLI `--gateway-socket` / `ACOWORK_GATEWAY_SOCKET` becomes a hidden deprecated alias
5. `AgentBootContext::socket_path` → `AgentBootContext::endpoint`
6. delete the `_socket_path: String` parameter of `run_gateway_loop`
7. Gateway `spawn_agent_process`: `--gateway-socket` → `--gateway-endpoint`, keeping
   `--gateway-socket` working as a compat alias
8. test fixture: `unix:///tmp/gateway.sock` → `http://127.0.0.1:19877`

### Commit C6: proto package + comment cleanup

1. `core/acowork-core/proto/gateway_ipc.proto:3`: `package acowork.ipc.v1;` → `package acowork.gateway.v1;`
   — prost generates fine via `tonic::include_proto!("acowork.gateway.v1")`; no
   `package_file_name` configuration needed.
2. `acowork-core/build.rs`: unchanged (it references the `.proto` path, not the package name).
3. ~20 comment cleanups across Gateway + Runtime: `IPC server` → `gRPC server`,
   `ipc_session_mgr` → `grpc_session_mgr`, `alternative to IPC transport` → `sole transport`,
   `legacy IPC client` → `legacy GatewayClient`, log strings updated accordingly.
   One historical note is deliberately kept: `gateway/src/handlers/server.rs:895`
   `no longer using legacy IPC transport`.
4. the proto **file name** `gateway_ipc.proto` is kept as-is (traceable via `git diff`; `build.rs`
   paths unchanged).

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Breakage of the push path after C2's Branch 2 removal | 🔴 high | audit all push callers; add a `tracing!` debug per path |
| A dispatch call site missed after C2's handler signature change | 🟡 medium | `cargo build` is a full compiler check — nothing is missed |
| C6's proto package rename breaks an external gRPC schema | 🟡 medium | flag as a breaking change in the CHANGELOG; only our own Runtime consumes it |
| C5's CLI parameter removal affects external scripts | 🟡 medium | keep `--gateway-socket` as a deprecation alias |
| cron session lookup semantics drift after switching to `agent_id` | 🟡 medium | add test coverage for the cron trigger scenario |
| `intent/router.rs` `session_mgr.lock().await` deadlock | 🟢 low | the old path was already a dual-lock; the change reduces lock count |

## Verification

After every commit:

```bash
cd core
cargo build --release       # 0 errors
cargo clippy --all-targets -- -D warnings  # 0 warnings
cargo test                  # all pass (except known failures)
```

Smoke test after C6:

- [x] `cargo build --release` passes
- [x] `cargo clippy` 0 warnings
- [x] `cargo test` all pass
- [ ] start the Gateway → starts cleanly, no "failed to bind socket" class errors
- [ ] Desktop connects → starts the System Agent → send a message, get a reply
- [ ] change the Provider API key → hot-push → Runtime logs `ProviderListUpdate`
- [ ] change the MCP config → hot-push → Runtime logs `SearchConfigDelivery`
- [ ] switch model → restart the Runtime → `AgentHello` handshake succeeds
- [ ] trigger `tool_approval_needed` manually → desktop dialog → approve → the Runtime receives it
- [ ] DevMode debug panel (HTTP RPC + MQTT events, ADR-048) works
- [ ] scan Gateway logs: no ERROR "missing session" / "unauthenticated session"

## Explicit Non-Goals

- **No Desktop App changes** — the desktop gRPC client (TypeScript) is independent code; this ADR
  only cleans the Rust side
- **`doc/design/zh/16-ipc-grpc-migration.md` is not modified** — it is the historical record of the
  migration and stays as-is
- **the `gateway_ipc.proto` file name is not changed** — only the in-proto `package` name
- **`build.rs` is not changed** — if the package rename does not affect prost's generated path, there
  is nothing to do
- **acowork-core's own `pub use` re-export chain keeps its names** — `acowork_core::protocol::GatewayRequest`
  and friends stay stable

## Follow-up Cleanup (out of ADR-031's scope, for a future ADR)

1. **Public API error variant `Ipc(String)`** in `acowork_core::AcoworkError::Ipc`,
   `acowork_gateway::AcoworkError::Ipc`, `acowork_runtime::RuntimeError::Ipc` — **decision: keep the
   name** (public API stability); renaming would be a breaking change requiring a major version bump.
2. **Public constant `SESSION_IPC`** in `acowork_core::timeout_config::constants::SESSION_IPC` and
   `acowork_gateway::http::chat::SESSION_IPC_TIMEOUT` — **decision: keep the name**, internal comments
   already updated to "gRPC".
3. **Test fixtures and deprecated aliases** — `runtime/src/cli.rs:3406`'s `test_cli_gateway_socket_arg`
   keeps the `unix:///tmp/gateway.sock` fixture as a deprecated-alias regression test;
   `apps/acowork-desktop/src/lib/types.ts:166`'s `GatewayConfig.socket_path` stays (per the
   Non-Goals), so the TypeScript type and the runtime `ConfigResponse` are slightly inconsistent —
   harmless because the field is `undefined`.
4. **C5b gap** — `DataFlowConfig::ipc_push_capacity` (defined at `acowork-gateway/src/config.rs:138-141`)
   was missed by C5 (0 consumers, semantically duplicated by `grpc_outbound_capacity`); deleted in a
   follow-up "C5b 补漏" commit.
5. **Implementation deviations** — `handlers/server.rs` rather than `handlers.rs` (a directory is more
   extensible once `session_state` is needed); `handlers/session_state.rs` never materialised (C2
   merged everything into `GrpcSession`); the C6 test fixture rewrite was skipped in favour of
   keeping the deprecated-alias fixture.

## Appendix: Complete File Change Matrix

| File | C1 | C2 | C3 | C4 | C5 | C6 |
|------|:--:|:--:|:--:|:--:|:--:|:--:|
| `gateway/src/ipc/server.rs` → `gateway/src/handlers/server.rs` | ✅ | ✅ | | | | |
| `gateway/src/ipc/session.rs` → `gateway/src/handlers/session_state.rs` | ✅ | ✅ | | | | |
| `gateway/src/ipc/mod.rs` | edit | ✅ | ❌del | | | |
| `gateway/src/ipc/global_push.rs` → `gateway/src/grpc/resource_pusher.rs` | | | ✅ | | | |
| `gateway/src/grpc/mod.rs` | ✅ | | ✅ | | | |
| `gateway/src/grpc/dispatch.rs` | ✅ | ✅ | | | | |
| `gateway/src/grpc/server.rs` | ✅ | ✅ | | | | |
| `gateway/src/handlers/mod.rs` | **new** | | | | | |
| `gateway/src/lib.rs` | ✅ | | | | | |
| `core/proto/gateway_ipc.proto` | | | | | | ✅ |
| `acowork-core/build.rs` | | | | | no change needed | |
| `acowork-core/src/lib.rs` | | | | | | ✅ |
| `gateway/src/gateway/{mod,state}.rs` | ✅ | ✅ | ✅ | | ✅(comments) |
| `gateway/src/http/{server,mod,chat,agents}.rs` | | | ✅ | | ✅(comments/logs) |
| `gateway/src/intent/router.rs` | ✅ | ✅ | | | ✅(comments) |
| `gateway/src/cron/mod.rs` | ✅ | ✅ | | | ✅(doc) |
| `runtime/src/agent/loop_.rs` | | | | | ✅(comments) |
| `runtime/src/grpc/client.rs` | | | | | ✅(doc) |
| Total files | ~16 | ~8 | ~8 | ~3 | ~8 | ~4 |
