# ADR-015: Agent Startup Sequencing Refactor — From Async Race to Phased Readiness

> **Chinese source of truth**: [ADR-015](../zh/ADR-015-agent-startup-sequencing.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft (pending implementation)

## Date

2026-06-19

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

- `core/acowork-runtime/src/cli.rs` (rearrange the `async_main` startup flow)
- `core/acowork-runtime/src/agent/session/session_manager.rs` (centralize SessionState assembly)
- `core/acowork-runtime/src/agent/session/session_task.rs` (degrade into a passive handler)
- `core/acowork-runtime/src/agent/loop_session.rs` (the `emit_session_state` call timing)
- `core/acowork-gateway/src/http/` (a new session-state pull endpoint)
- `apps/acowork-desktop/src/lib/agent-start.ts` (`syncAgentUI` gains `fetchSessionState`)
- `apps/acowork-desktop/src/stores/chatStore.ts` (actively pull state on first entry to a session)

---

## Context

The frontend session-state panel (ResultsPanel) needs to display the session's current `model`,
`provider`, `reasoning_effort` (thinking level), `temperature`, `workspace_id` and `ratio`
(characters/token) as soon as the agent starts. In the current implementation, however, **on the
first entry into a session after a cold start the Thinking Level column keeps showing `off` until
the user manually switches models**. Multiple attempts to locate and fix it did not eliminate the
problem — the root cause lies not in the initialization logic of any specific field but in **the
design of the entire agent startup sequencing**.

### Problem 1: `SessionTask`'s initial emit precedes both chunk_relay and the frontend WebSocket connection

The current sequence (`cli.rs::async_main`):

```text
T=0    AgentHello → AgentHelloResult                    main thread
T=10   AgentCore::new + global_provider_list injection  main thread
T=20   SessionManager::create_session_with_id_and_conversation
         └─ tokio::spawn(SessionTask::run)              ⚡ async fork point
              T=20.1  SessionTask initializes reasoning_effort internally
              T=20.2  SessionTask::emit_session_state()  ← the first push
                       └─ chunk_tx.try_send(SessionStateChanged)
                          (nobody is recv'ing on chunk_rx yet, the event is buffered)
              T=20.3  SessionTask enters the inbound loop
T=21   main thread: route_model_switch / SetWorkDir / UpdateRuntimeConfig
        each message may trigger SessionTask to emit_session_state again
T=40   AgentReady → Gateway sets ready=true
T=41   spawn chunk_relay → starts chunk_rx.recv()
        └─ dumps the buffered SessionStateChanged events to outbound in one go
T=50   run_gateway_loop ("Gateway message loop started")
T≥500  Desktop App waitForAgentReady polling finds ready=true
T≥501  Desktop App connectStream() establishes the WebSocket
        ⚠ at this point chunk_relay has long since sent the initial session_state_changed
           intent to the Gateway; if the Gateway does not cache the latest state,
           the frontend never receives it.
```

**The core symptom**: whether the frontend can obtain the initial state depends entirely on the
relative timing of the WebSocket connection versus the chunk_relay startup — a race condition that
should not exist at all.

### Problem 2: the semantics of `AgentReady` are muddled

`AgentReady` is currently sent at `cli.rs:1269` (Step 10), at which point:
- ✅ AgentCore is ready
- ✅ SessionManager has been created
- ⚠ **SessionTask is spawned but its state is not yet stable** (the main thread will keep sending
  it ModelSwitch / SetWorkDir / UpdateRuntimeConfig)
- ❌ **chunk_relay is not yet spawned** (that happens in Step 11)
- ❌ MCP is still connecting in the background (up to 30s)
- ❌ **run_gateway_loop has not started yet** (Step 12)

The real semantics of `ready` is "the main thread finished part of its synchronous
initialization", not "the Runtime is fully ready and can receive and respond to any frontend
request". Once the frontend sees `ready=true` it immediately establishes the WebSocket and falls
into the race window of Problem 1.

### Problem 3: per-session state is written in parallel from several places

The `reasoning_effort` initialization logic exists in four different places:

| Location | Timing |
|---|---|
| `SessionManager::create_session_with_id_*` (L312) | at session creation |
| `SessionTask::run` startup initialization (L468) | immediately after spawn |
| `SessionTask` handling `ProviderListUpdated` (L1073) | on async provider-list update |
| `SessionTask` handling ModelSwitch (L1083, L1182) | on model switch |

Four pieces of code do approximately the same thing (read the default from the model capabilities
→ parse → set into SessionState), but their execution order is driven by async messages, so final
consistency cannot be guaranteed. `temperature` is likewise scattered across SessionState,
AgentCore and runtime_overrides.

### Problem 4: the frontend depends on push for the initial state

The current architecture assumes "SessionTask emits the state once at startup and the frontend
receives it over the WebSocket". This design has two fundamental flaws:
1. push is fire-and-forget — the sender cannot confirm the receiver is ready;
2. in multi-session scenarios (the user switching tabs) an explicit emit must be triggered to get
   that session's current state, making the logic redundant.

### Problem 5: the cost of the temporary fixes

Without refactoring the startup sequencing, the fix attempts so far have included: proactively
initializing `reasoning_effort` at the start of `SessionTask::run`; introducing a
`SessionMessage::ProviderListUpdated` broadcast; adding lazy-init inside `emit_session_state`
(rejected and rolled back); and adding a large amount of tracing to locate lost events. Each treats
the symptom — until the core sequencing problem is solved, the next per-session field added will
repeat the same mistake.

## Decision

Refactor the agent startup sequencing into **two phases of synchronous initialization +
`AgentReady` at the end + an actively pulling frontend**. `SessionTask` no longer owns the
responsibility of "emitting the initial state at startup"; the frontend pulls the snapshot through
the new `GET /api/agents/{agent_id}/sessions/{session_id}/state` endpoint.

### Core principles

1. **Strict phasing of per-agent and per-session** — Phase A completes all cross-session shared
   initialization (provider list, key vault, tools, embedding, memory store); only then does Phase B
   create sessions.
2. **SessionState is assembled completely synchronously on the main thread** — `reasoning_effort` /
   `temperature` / workspace / history are all completed inside `SessionManager` before the
   `tokio::spawn`, so a spawned `SessionTask` receives complete state.
3. **SessionTask degrades into a passive message handler** — delete the "initialize + emit at
   startup" logic; handle only runtime events (user message, ModelSwitch, debug).
4. **`AgentReady` is a true readiness signal** — sent after all synchronous initialization is
   complete, chunk_relay is spawned, and run_gateway_loop is about to enter its message loop;
   its semantics are "the Runtime is fully ready".
5. **Snapshots go over pull, changes go over push** — the frontend actively pulls the initial
   state; runtime changes (streaming, status transitions, the new state after ModelSwitch)
   continue over chunk_relay push.

### Sequence comparison

**Before the refactor (async race)**:

```text
async_main main thread:
  AgentHello → assembly → SessionManager::create_session
    └─ tokio::spawn(SessionTask::run) ⚡ fork
                     ├─ initialize reasoning_effort
                     ├─ emit_session_state ← buffered into chunk_tx
                     └─ enter the inbound loop
  → route_model_switch / SetWorkDir / UpdateRuntimeConfig
  → AgentReady (muddled semantics: actually not ready)
  → spawn chunk_relay
  → run_gateway_loop
```

**After the refactor (phased readiness)**:

```text
async_main main thread (all synchronous):
  ── Phase A: per-agent initialization ──
    AgentHello → AgentHelloResult
    build system_prompt / SkillRegistry / tools / embedding
    AgentCore::new + inject global_provider_list / key_vault / memory_session
    init_memory_store
    SessionManager::new

  ── Phase B: per-session initialization (synchronous, no spawn) ──
    load the conversation (resume the latest / create a new session)
    validate provider/model + fallback
    build the complete SessionState (model, provider, reasoning_effort,
                           temperature, workspace_id, history, MCP tools)
    persist the SessionState header into the JSONL (only when a fallback correction occurred)

  ── Phase C: start subsystems ──
    tokio::spawn(SessionTask::run)        ← passive handler, no longer emits
    spawn(chunk_relay)                     ← both ends of the channel are ready by now
    spawn(MCP background connection)       ← background async, non-blocking

  ── Phase D: announce readiness ──
    AgentReady → Gateway: agent.ready = true
    run_gateway_loop("ready to receive inbound messages")

Desktop App:
  waitForAgentReady (polling) → connectStream (WS)
                            → fetchSessionState (HTTP pull, the full snapshot)
                            → subsequent changes arrive as WS push incremental updates
```

## Implementation Steps

7 phases in total. Each is independently compilable, manually testable and non-breaking to
existing functionality. One commit per phase is recommended for easy rollback.

### Phase dependencies

Phase 0 → 1 → 2 → 3 → 4 → 5 → 6. Phases 0–3 **must be implemented strictly in order**, not in
parallel. Phase 4 (the pull endpoint) and Phase 5 (the frontend integration) can proceed in
parallel once Phase 3 is done. Phase 6 must come last.

### Phase 0: split `async_main` into phase functions

**Goal**: turn the 3,600+-line `async_main` into a clear phase orchestrator plus independent phase
functions, making the startup flow obvious at a glance.

**Files**: `core/acowork-runtime/src/cli.rs` (split into an orchestrator + phase functions);
optionally a new `core/acowork-runtime/src/startup/` module directory.

**Core design**: introduce an `AgentBootContext` struct as the data carrier between phases,
replacing a dozen scattered local variables:

```rust
/// Intermediate context produced by Phase A, consumed by subsequent phases.
struct AgentBootContext {
    package: LoadedPackage,
    grpc_client: GatewayGrpcClient,
    hello_config: AgentHelloConfig,
    provider: Arc<dyn LLMProvider>,
    embedding: Arc<dyn EmbeddingProvider>,
    tool_registry: ToolRegistry,
    skill_registry: SkillRegistry,
    chunk_tx: ChunkSender,
    chunk_rx: Option<ChunkReceiver>,
    // ... other per-agent resources
}
```

The refactored `async_main` becomes a ~20-line orchestrator:

```rust
async fn async_main(config: RuntimeConfig, ...) -> Result<()> {
    // Phase A: per-agent resources
    let agent_ctx = phase_a_init_agent(&config).await?;

    // Phase B: per-session state (synchronous assembly)
    let session_ctx = phase_b_init_session(&agent_ctx, &config).await?;

    // Phase C: spawn subsystems
    let subsystems = phase_c_spawn_subsystems(&agent_ctx, &session_ctx).await?;

    // Phase D: announce ready & enter the loop
    phase_d_run(&agent_ctx, &session_ctx, subsystems).await
}
```

Each phase function is 200–400 lines, single-responsibility and independently testable. An
optional option is moving the phase functions into a dedicated `startup/` submodule:

```
src/
├── cli.rs              # only arg parsing + async_main orchestration (~100 lines)
├── startup/
│   ├── mod.rs          # re-exports
│   ├── context.rs      # the AgentBootContext definition
│   ├── agent_init.rs   # Phase A
│   ├── session_init.rs # Phase B
│   ├── subsystems.rs   # Phase C (chunk_relay, MCP)
│   └── gateway_loop.rs # Phase D
```

**Standalone mode**: Phases A/B are common to both Gateway and Standalone modes; Phases C/D fork
by mode. Standalone mode does not spawn chunk_relay in Phase C and Phase D enters `run_chat_loop`
directly.

**Verification**: `cargo build -p acowork-runtime`; the `async_main` body < 50 lines; each phase
function compiles independently.

### Phase 1: extract the SessionState assembly into a SessionManager helper

**Goal**: centralize the "assemble SessionState" logic currently scattered across
`create_session_with_id_and_conversation`, `SessionTask::run` startup initialization, the
`ProviderListUpdated` handler and the `ModelSwitch` handler into one `SessionManager` helper.
Phase 1 only extracts; it does not change the sequencing.

**File**: `core/acowork-runtime/src/agent/session/session_manager.rs`

**New function**:

```rust
/// Build a fully-initialized SessionState for a new or resumed session.
/// All per-session fields are set synchronously before this returns.
/// Caller must hold an Arc<AgentCore> with global_provider_list populated.
fn build_initial_session_state(
    &self,
    conversation: Option<&ConversationSession>,
) -> SessionState
```

**Logic moved in**: the new SessionState + set_model/provider (L287-316);
`history_mut().set_max_tokens(context_trim_budget)` (L301-302); parsing
`default_reasoning_effort` from the model capabilities and setting it (L306-312); and the
currently-missing read of the temperature override from AgentCore or the SessionManager cache.

**Verification**: `cargo build -p acowork-runtime` + `cargo test -p acowork-runtime --lib
session_manager` green; cold-start behavior identical to before (the `reasoning_effort=off` bug
still exists but is not worse).

### Phase 2: `SessionTask::run` drops startup initialization and `emit_session_state`

**Goal**: SessionTask becomes a purely passive handler. The SessionState it receives is already
fully assembled by the main thread.

**File**: `core/acowork-runtime/src/agent/session/session_task.rs`

**Deleted**: the whole block initializing `reasoning_effort` from model capabilities (L449-469);
the whole block calling `agent_loop.emit_session_state()` at startup (L471-481); and the duplicated
`reasoning_effort` initialization inside `ProviderListUpdated` and `ModelSwitch` (L1073, L1083,
L1182) — keeping ModelSwitch's "reset from the new model's capabilities after switching" logic but
converging it into one shared `apply_model_defaults` private method.

**Retained emit trigger points**: after each `transition_status` (inside `loop_session.rs`); after
a ModelSwitch completes (one manual `emit_session_state`); after SetWorkDir completes. All of these
occur after chunk_relay is ready, so the events flow to the frontend normally.

**Verification**: `cargo build -p acowork-runtime`; on agent startup the log no longer prints
`SessionTask: initializing reasoning_effort`; state changes (e.g. streaming/idle transitions after
sending a message) still reach the frontend as `session_state_changed`.

### Phase 3: rearrange the startup order in `cli.rs`, moving `AgentReady` to the end

**Goal**: split `async_main` into four clear phases executed in the order A→B→C→D, sending
`AgentReady` last.

**File**: `core/acowork-runtime/src/cli.rs`

| Phase | Current location | New location / adjustment |
|---|---|---|
| A | L313-961 | unchanged (package, hello, provider, tools, embedding, AgentCore, SessionManager::new) |
| B | L769-851 + L993 | concentrate the conversation loading, provider/model validation and create_session after Phase A |
| B+ | L1054-1131 | `workspace_context` and `agent_config` overrides are passed in **as arguments** before create_session |
| B+ | L1009-1033 | delete `route_model_switch` (after Phase 1 the SessionState assembly already uses the correct provider) |
| C | L1296-1660 | unchanged: spawn chunk_relay |
| C | L1187-1239 | unchanged: spawn the MCP background connection |
| D | L1269-1294 | **moved after L1660**: send AgentReady |
| D | L1544-1730 | unchanged: enter run_gateway_loop |

**Key code changes**:

```rust
// NEW: before Phase B, read the overrides from agent_config.json and
// hand them to build_initial_session_state via the SessionManager cache
let agent_overrides = load_agent_config(work_dir_path).unwrap_or_default().unwrap_or_default();
session_manager.set_runtime_overrides_cache(agent_overrides.clone());

// Phase B: pass the overrides and workspace_context in when creating the session
let initial_session_id = session_manager
    .create_session_with_id_and_conversation(sid.clone(), conversation)
    .await?;

// Phase C: spawn chunk_relay
let chunk_relay = tokio::spawn(...);

// Phase D: announce ready only after everything is ready
client.outbound_sender().send(AgentReady{...}).await?;
run_gateway_loop(...).await
```

**Verification**: in the logs `AgentReady sent to Gateway` appears later than
`Chunk relay started`; the frontend's `waitForAgentReady` wait grows slightly (~100–200ms,
acceptable); after a cold start no `session_state_changed` event is dropped.

### Phase 4: the Runtime → Gateway → frontend session-state pull endpoint

**Goal**: let the frontend pull a full SessionState snapshot after the WebSocket connects.

**Files**: `core/acowork-runtime/src/cli.rs` (handle the new `GetSessionState` request in
`gateway_recv`); `core/acowork-runtime/src/agent/session/session_manager.rs` (add
`snapshot_session_state(&self, session_id: &str) -> Option<SessionStateSnapshot>`);
`core/acowork-core/src/proto/` (new `GetSessionStateRequest` / `GetSessionStateResponse` proto
messages); `core/acowork-gateway/src/http/chat.rs` or a new `session_state.rs` (the new
`GET /api/agents/{agent_id}/sessions/{session_id}/state` route);
`core/acowork-gateway/src/grpc/dispatch.rs` (route `GetSessionState` to the right Runtime).

```rust
#[derive(Serialize)]
pub struct SessionStateSnapshot {
    pub session_id: String,
    pub status: SessionStatus,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub workspace_id: Option<String>,
    pub ratio: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
}
```

**Key implementation point**: `SessionManager::snapshot_session_state` reads directly from the
SessionState snapshot held by the `SessionHandle`. **Note**: `SessionTask` also reads and writes
SessionState, so the assembly phase (main thread) and `SessionTask` must share a single visible
view of it.

**Preferred option**: extract the snapshot fields (model / provider / reasoning_effort /
temperature / workspace_id — only ~10 lightweight fields) into an
`Arc<RwLock<SessionStateSnapshot>>` that `SessionTask` writes on every state change. Rationale:
SessionState is a large struct (containing history and tool results) and locking it as a whole
introduces unnecessary contention; an independent `SessionStateSnapshot` has a small change surface,
performs well, and is sufficient for the pull endpoint's read needs.

**Alternative**: if the snapshot fields turn out to be insufficient, consider changing the whole
`SessionState` to shared ownership via `Arc<RwLock<SessionState>>` — but note that locking a large
struct as a whole introduces unnecessary contention.

**Verification**: `curl http://127.0.0.1:19876/api/agents/com.acowork.senior-engineer/sessions/{sid}/state`
returns the full JSON snapshot; repeated calls return the latest values; a non-existent session
returns 404.

### Phase 5: Desktop App wires in `fetchSessionState`

**Goal**: after `syncAgentUI` sees `agent.ready=true`, the frontend first establishes the
WebSocket, then actively pulls the initial session state.

**Files**: `apps/acowork-desktop/src/lib/agent-start.ts` (add `fetchInitialSessionState`);
`apps/acowork-desktop/src/stores/chatStore.ts` (add a `fetchSessionState(agentId, sessionId)`
action mapping the response to `SessionChatState`);
`apps/acowork-desktop/src/stores/sessionStore.ts` (if needed, also call `fetchSessionState` when
switching session tabs).

```typescript
// agent-start.ts
export async function syncAgentUI(agentId: string) {
  await useAgentStore.getState().waitForAgentReady(agentId);
  useChatStore.getState().connectStream(agentId, getGatewayUrl());

  // NEW: pull the initial session state for the active session
  const activeSid = useChatStore.getState().getActiveSessionId(agentId);
  if (activeSid) {
    await useChatStore.getState().fetchSessionState(agentId, activeSid);
  }

  useWorkspaceStore.getState().fetchWorkspaces(agentId);
  emitAgentConfigRefresh(agentId);
}
```

**Verification**: on agent cold start, entering a session immediately shows the correct Thinking
Level (no longer `off`); switching session tabs also immediately obtains that session's state; a
`GET /api/agents/.../state` request is visible in the network inspector.

### Phase 6: clean up the temporary fixes and redundant logging

**Files**: `core/acowork-runtime/src/agent/session/session_task.rs` (delete the redundant
`reasoning_effort` initialization logging inside the `ProviderListUpdated` handling);
`core/acowork-runtime/src/agent/session/session_manager.rs` (evaluate whether the broadcast in
`update_global_provider_list` is still needed — if the provider list is no longer hot-updated at
runtime it can go); `core/acowork-runtime/src/agent/loop_session.rs` (delete the warn on
`try_send` failure or downgrade it to debug — since SessionTask no longer emits at startup,
`try_send` failure mainly happens when the channel is closed, which does not occur on the normal
path); **keep** the `SessionMessage::ProviderListUpdated` message variant — it is required at
runtime: when the frontend modifies the provider list (e.g. adding/removing API keys) it must
notify all live SessionTasks in real time to update the available model list. Phase 6 does not
delete this variant, only cleans up its **redundant `reasoning_effort` re-initialization logic** —
change it to only update the available model list without resetting the current session's
`reasoning_effort`.

**Verification**: `cargo clippy --all-targets -- -D warnings` green; `cargo test` green; the three
scenarios (cold start, model switch, multi-session switching) all work.

## Rejected Alternatives

### A. Cache the latest session state on the Gateway side

Have the Gateway cache the latest value when it receives a `session_state_changed` intent, and push
that cached state once when the frontend's WebSocket connects.

**Rejected because**: it adds a stateful dependency to the Gateway, violating its design as a
"stateless router" (see `module-design/zh/03-gateway.md`); with multiple sessions the Gateway must
maintain one cache per session, duplicating SessionManager's SessionState; it still does not fix
"the main thread keeps pushing state changes after SessionTask starts"; and it does not address how
to obtain the state of a non-active session when switching tabs.

### B. Have the `AgentReady` payload carry the initial SessionState snapshot

Extend the AgentReady proto to carry `initial_session_id` and the full `SessionState` snapshot,
passed through by the Gateway to the HTTP `/api/agents` response.

**Rejected because**: AgentReady changes from a "flag" to a "data carrier", making its semantics
heavier; with multiple sessions it can only carry one initial session's snapshot and the others
still need pulling — in that case it is simpler to uniformly use pull; it couples to the existing
`/api/agents` list endpoint with a larger change surface than this ADR's approach; and the pull
approach's extra latency is < 50ms, imperceptible to users.

### C. Give SessionTask a "ready" signal and have the main thread wait for it

Have `SessionTask` notify the main thread via a oneshot channel during `run()` that it is ready,
and let the main thread wait for that signal before sending `AgentReady`.

**Rejected because**: the definition of "ready" inside SessionTask is fuzzy (did it finish emitting
the initial state? did it enter the inbound loop?); the frontend still needs a pull endpoint for the
multi-session scenario; it adds a cross-task synchronization primitive (oneshot) without actually
moving the SessionState assembly onto the main thread, so the problem is not solved; and it
contradicts this ADR's core principle that "assembly completes synchronously on the main thread".

### D. Don't refactor; hardcode the `reasoning_effort` default to Medium

The simplest fallback: always default `reasoning_effort` to `Some(ReasoningEffort::Medium)` in
`SessionState::new`, never depending on model capabilities.

**Rejected because**: it permanently loses the "the model's own recommended effort" semantics
(e.g. some reasoning models should default to High and weak models to Off); the model capabilities
must still be read when they explicitly specify a `default_reasoning_effort`, so the bug remains;
it does not solve the same class of problem for other per-session fields such as `temperature` and
`workspace_context`; and it is a textbook treat-the-symptom approach, contradicting the user's
explicit "sort out the sequencing" requirement.

## Risks and Rollback

**Risk 1 — restructuring the whole `SessionState` ownership (Phase 4) has a large change
surface.** SessionState is currently exclusively held by `SessionTask`; for the main thread's
`snapshot_session_state` to read it, it must become `Arc<RwLock<SessionState>>` or similar shared
ownership. If restructuring SessionState directly is too risky, the **fallback** is maintaining an
independent `Arc<RwLock<SessionStateSnapshot>>` in `SessionHandle` (only the ~10 snapshot fields)
written synchronously by `SessionTask` on every `emit_session_state`.

**Risk 2 — missing an existing emit call site.** After Phase 2 deletes SessionTask's startup emit,
every emit call site for "runtime state changes" must still exist. List all `emit_session_state`
call sites in the PR and review them one by one.

**Risk 3 — Phase 3 changes how `workspace_context` / `runtime_overrides` are assembled.** Today
they are delivered to the session after creation via broadcast messages; after the refactor they
become creation-time arguments. Carefully review the callers of
`update_session_workspace_context` and `apply_runtime_config_override` to ensure no initialization
path is missed.

**Rollback strategy**: each phase is an independent commit that can be reverted individually. The
most dangerous are Phase 3 (the `cli.rs` rearrangement) and Phase 4 (the snapshot endpoint) —
each should be its own PR with independent review and testing.

## Supplementary Design Constraints

**Constraint 1 — a single entry point `SessionManager::create_session_complete`.** The core logic
of Phase B should sink into `SessionManager` as `create_session_complete()`, completing "load the
conversation → validate provider/model → assemble the full SessionState → register the handle" in
one pass. This means: `cli.rs`'s Phase B needs a single call; later user-created sessions follow the
same path, avoiding the code split between "the initial session goes through the cli.rs path" and
"later sessions go through the SessionManager path"; and `route_model_switch`,
`update_session_workspace_context` and `apply_runtime_config_override` are folded in as arguments or
internal steps of `create_session_complete`.

**Constraint 2 — mark each phase with a tracing span.** Each phase function entry uses
`info_span!` so the logs naturally show the phase division and timings:

```rust
async fn phase_a_init_agent(config: &RuntimeConfig) -> Result<AgentBootContext> {
    let _span = tracing::info_span!("startup_phase_a").entered();
    // ...
}
```

**Constraint 3 — a timeout guard for Phase B.** Phase B involves disk I/O (conversation loading)
and possibly network validation (provider validation), so a reasonable timeout (10s recommended) is
required to avoid blocking the whole startup. On timeout, fall back to the default state rather
than panicking.

**Constraint 4 — integration test coverage.** Before the refactor, add an end-to-end integration
test verifying the complete "cold start → frontend obtains the correct session state" chain.
Recommended: a new `startup_sequencing_test.rs` under `core/tests/` covering at least: the session
state snapshot after a cold start contains the correct `reasoning_effort`; the AgentReady
timestamp is later than the chunk_relay spawn timestamp; and the pull endpoint returns a state
consistent with the push events.

## Acceptance Criteria

- [ ] On agent cold start, entering the session panel the first time shows the correct Thinking Level immediately (no longer `off`)
- [ ] On agent cold start, entering the session panel the first time shows the correct Temperature immediately (default 0.70 or the model override)
- [ ] After a model switch (ModelSwitch), all fields in the state panel update within 1s
- [ ] Switching session tabs immediately reflects that session's real state
- [ ] In the backend logs: `AgentReady sent to Gateway` timestamp > `Chunk relay started` > `SessionManager: created new session`
- [ ] In the backend logs: the `SessionStateChanged event dropped` warning no longer appears
- [ ] `cargo clippy --all-targets -- -D warnings` green
- [ ] `cargo test` green
- [ ] `npx tsc --noEmit` (frontend) green
- [ ] The `async_main` body < 50 lines (containing only phase orchestration calls)
- [ ] Each phase function can be compiled and unit-tested independently
- [ ] The startup log shows the four tracing spans `startup_phase_a`, `startup_phase_b`, `startup_phase_c`, `startup_phase_d` with their respective timings

## References

- Current implementation: `core/acowork-runtime/src/cli.rs` (`async_main`, 4020 lines);
  `core/acowork-runtime/src/agent/session/session_task.rs` (`SessionTask::run`);
  `core/acowork-runtime/src/agent/session/session_manager.rs`
  (`create_session_with_id_and_conversation`); `core/acowork-runtime/src/agent/loop_session.rs`
  (`emit_session_state`)
- Preceding ADRs: ADR-012 (per-session model isolation) — the design moving the model from global to
  per-session; ADR-013 (Debug Observer Pipeline) — a similar "assemble synchronously on the main
  thread + passively consume after spawn" pattern; ADR-014 (AgentLoop module decomposition) — the
  implementation methodology for phased refactoring
- Module design docs: `docs/module-design/zh/02-runtime.md` (the Runtime module design);
  `docs/module-design/zh/03-gateway.md` (the Gateway's stateless-router principle)
