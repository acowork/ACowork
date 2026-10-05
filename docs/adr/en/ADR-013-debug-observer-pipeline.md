# ADR-013: Refactoring the Debug Module Boundary — The Observer Pipeline Pattern

> **Chinese source of truth**: [ADR-013](../zh/ADR-013-debug-observer-pipeline.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Proposed

## Date

2026-06-05

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

`agent_core.rs`, `loop_.rs`, `context.rs`, `session_task.rs`, `session_manager.rs`,
`session_handle.rs`, the `debug/` module

---

## Context

During implementation, ACowork Runtime's Debug (DevMode) functionality gradually seeped into
non-debug modules, blurring the boundary between debug code and production code. The concrete
problems:

### Problem 1: severe intrusion into the main loop

`loop_.rs` is the worst offender — `execute_single_iteration` has **15+ debug call sites** and 5
debug-specific methods (`await_debug_resume`, `update_debug_phase`, `push_debug_step`,
`debug_auto_pause_if_stepping`, `capture_context_snapshot`), totalling **400+ lines** of debug code
scattered across a 4189-line file.

The intrusion patterns repeat:

```rust
// Pattern A: phase tracking — appears 6 times
self.update_debug_phase(DebugPhase::Xxx).await;

// Pattern B: step push + auto-pause — appears 4 times
self.push_debug_step(phase, input, output);
self.debug_auto_pause_if_stepping().await;

// Pattern C: guard conditions — appears 10+ times
if let Some(ctrl) = self.core.debug_ctrl() {
    // debug-only logic
}
```

These calls make the normal execution flow hard to read and understand — you cannot tell at a
glance "is this business logic or a debug hook?"

### Problem 2: AgentCore becomes a dumping ground for debug fields

`AgentCore` holds **6 `Option<T>` debug fields** plus **6 debug methods**:

```rust
pub(crate) debug_ctrl: Option<Arc<Mutex<DebugController>>>,
pub(crate) pending_debug_handles: Option<Arc<Mutex<Option<DebugHandles>>>>,
pub(crate) debug_rewind_notify: Option<Arc<Notify>>,
pub(crate) debug_resume_notify: Option<Arc<Notify>>,
pub(crate) debug_event_tx: Option<DebugEventSender>,
// + set_debug_mode(), check_and_apply_pending_debug(),
//   debug_ctrl(), debug_rewind_notify(), debug_resume_notify(), debug_event_tx()
```

These fields make `AgentCore` act as a debug state container, violating single responsibility. The
nested `Arc<Mutex<Option<...>>>` of `pending_debug_handles` is especially hard to read — it is a
product of bypassing the injection mechanism and exposes implementation details.

### Problem 3: ContextBuilder's debug hooks are mixed with business logic

`ContextBuilder` has **8 methods marked "for debug patching"**, plus `apply_patches()` and 7 section
accessors. More importantly, the `environment_override` field changes a normal branch in `build()`:

```rust
// the branch in context.rs build()
if let Some(ref env) = self.environment_override {
    // the debug override takes priority over auto-detection
} else {
    // normal logic
}
```

This means debug not only adds methods but also **modifies existing behavior**, so inference on the
non-debug path must also understand the debug path.

### Problem 4: rewind logic is spread across three files

The `apply_debug_rewind` function chain spans `session_task.rs` (3 functions + 2 call sites) and
`loop_.rs` (called inside `await_debug_resume`). rewind's state dependencies —
`DebugController.rewind_target`, `conversation_snapshots`, `HistoryManager.truncate_to()` — are
scattered across controller, loop and session_task with no unified boundary.

### Problem 5: debug message type coupling in SessionTask

`SessionMessage::EnableDebugMode(DebugHandles)` is a debug-specific message variant, polluting the
`SessionMessage` enum with the debug domain. `SessionTask` also holds 3 debug fields and 7 debug
call sites.

## Decision

Introduce the **Observer Pipeline pattern**, extracting debug functionality out of the main
execution flow into pluggable observers injected through a unified hook interface, rather than
scattering `if let Some(ctrl) = self.core.debug_ctrl()` guards across business code.

### Core Design

#### 1. The `DebugObserver` trait — the single abstract boundary for debug functionality

```rust
// debug/observer.rs

/// Pluggable observer for agent loop lifecycle events.
///
/// In production mode, a no-op implementation is used (zero-cost abstraction
/// via enum dispatch, not dynamic dispatch). In DevMode, the real
/// DebugController-backed observer is injected.
///
/// All methods have default no-op implementations so that implementing
/// only the needed hooks is ergonomic.
pub trait DebugObserver: Send + Sync {
    // ── Lifecycle ──

    /// Called at the start of each iteration, before budget check.
    fn on_iteration_start(&self, _iteration: u32, _history_len: usize) {}

    /// Called after the agent loop has been resumed from a pause.
    fn on_resume(&self) {}

    // ── Phase tracking ──

    /// Called when the agent loop enters a new phase.
    /// Returns true if a breakpoint was hit (caller should await resume).
    async fn on_phase_enter(&self, _phase: DebugPhase) -> bool { false }

    /// Called after a phase completes with its result.
    fn on_phase_step(&self, _phase: DebugPhase, _input: Option<Value>, _output: Option<Value>) {}

    /// Called after a phase completes; auto-pauses if in stepping mode.
    async fn on_phase_step_done(&self) {}

    // ── Context ──

    /// Called after ContextBuilder::build() completes.
    /// Captures a snapshot of the built context.
    async fn on_context_built(&self, _snapshot: ContextSnapshotRequest) {}

    /// Apply any pending patches to the context builder.
    /// Returns true if patches were applied.
    fn apply_pending_patches(&self, _builder: &mut ContextBuilder) -> bool { false }

    // ── Pause / Resume / Rewind ──

    /// Block until the debugger resumes execution.
    /// Returns false if the agent should stop.
    async fn await_resume(&self) -> bool { true }

    /// Check for pending rewind operations and apply them.
    async fn apply_rewind(&self, _history: &mut HistoryManager) {}

    // ── Runtime injection ──

    /// Check for bypass-injected debug handles (called each iteration start).
    fn check_pending_injection(&self) {}
}
```

#### 2. `DebugObserverSlot` — enum dispatch for the zero-cost abstraction

Instead of `Option<Box<dyn DebugObserver>>` (dynamic dispatch + heap allocation), use enum
dispatch:

```rust
// debug/observer.rs

/// Slot that holds either a real debug observer (DevMode) or a no-op.
/// Enum dispatch ensures zero overhead in production mode — the compiler
/// sees through the variant and eliminates dead code.
pub enum DebugObserverSlot {
    Production,
    Dev(DebugObserverImpl),
}

impl DebugObserverSlot {
    /// Delegate to the inner observer (or no-op for the Production variant).
    pub fn on_iteration_start(&self, iteration: u32, history_len: usize) {
        match self {
            DebugObserverSlot::Production => {}
            DebugObserverSlot::Dev(obs) => obs.on_iteration_start(iteration, history_len),
        }
    }

    // ... the same delegation for all trait methods
}
```

**Why an enum instead of `Option<Box<dyn>>`?** Enum dispatch is static and the compiler can perform
dead-code elimination on the `Production` variant; it needs no `alloc` and introduces no vtable
indirection; `match` branch prediction is nearly free on modern CPUs; and the method signatures are
visible, which is friendly to IDE completion.

#### 3. `DebugObserverImpl` — the real DevMode implementation

```rust
// debug/observer_impl.rs

/// Real debug observer backed by DebugController, event sender, and notify handles.
pub struct DebugObserverImpl {
    ctrl: Arc<Mutex<DebugController>>,
    event_tx: DebugEventSender,
    rewind_notify: Arc<Notify>,
    resume_notify: Arc<Notify>,
    pending_injection: Option<Arc<Mutex<Option<DebugHandles>>>>,
}
```

This struct converges the 5 `Option<T>` debug fields currently scattered across `AgentCore` into a
single `DebugObserverSlot` field. All debug logic (phase tracking, snapshot capture, breakpoint
checks, pause/resume, rewind) is encapsulated here.

#### 4. AgentCore simplification

**Before** (6 Option fields + 6 methods):

```rust
pub struct AgentCore {
    // ... business fields ...
    pub(crate) debug_ctrl: Option<Arc<Mutex<DebugController>>>,
    pub(crate) pending_debug_handles: Option<Arc<Mutex<Option<DebugHandles>>>>>,
    pub(crate) debug_rewind_notify: Option<Arc<Notify>>,
    pub(crate) debug_resume_notify: Option<Arc<Notify>>,
    pub(crate) debug_event_tx: Option<DebugEventSender>,
}
```

**After** (1 field):

```rust
pub struct AgentCore {
    // ... business fields ...
    pub(crate) debug_observer: DebugObserverSlot,
}
```

All debug accessor methods are deleted and replaced with `self.debug_observer.on_xxx()` calls.

#### 5. Main-loop simplification

**Before** — every injection point has an explicit guard:

```rust
// the start of an iteration
self.core.check_and_apply_pending_debug();
let debug_iter = if let Some(ctrl) = self.core.debug_ctrl() {
    let mut ctrl = ctrl.lock().await;
    ctrl.iteration += 1;
    let msg_count = self.session.history.len();
    ctrl.create_conversation_snapshot(msg_count, usage);
    Some(ctrl.iteration)
} else {
    None
};
if !self.await_debug_resume().await {
    return Ok(IterationResult::Stopped(String::new()));
}
if let Some(ctrl) = self.core.debug_ctrl() {
    let mut ctrl_guard = ctrl.lock().await;
    if let Some(patches) = ctrl_guard.pending_patches.take() {
        context_builder.apply_patches(&patches);
    }
}

// phase tracking
self.update_debug_phase(DebugPhase::BudgetCheck).await;
// ... business logic ...
self.update_debug_phase(DebugPhase::BuildContext).await;
self.capture_context_snapshot(context_builder, debug_iter, &current_model).await;
// ... business logic ...
self.update_debug_phase(DebugPhase::LlmCall).await;
// ...
self.push_debug_step(DebugPhase::Idle, None, output);
self.debug_auto_pause_if_stepping().await;
```

**After** — the guard is internalized into the observer and the main loop only sees semantically
clear hook calls:

```rust
// the start of an iteration
self.core.debug_observer.check_pending_injection();
let debug_iter = self.core.debug_observer.on_iteration_start(
    /* iteration */, self.session.history.len()
);
if !self.core.debug_observer.await_resume(&mut self.session).await {
    return Ok(IterationResult::Stopped(String::new()));
}
self.core.debug_observer.apply_pending_patches(context_builder);

// phase tracking — a single call, no guard needed
if self.core.debug_observer.on_phase_enter(DebugPhase::BudgetCheck).await {
    // breakpoint hit, already handled inside the observer
}
// ... business logic ...
if self.core.debug_observer.on_phase_enter(DebugPhase::BuildContext).await {
    // breakpoint hit
}
self.core.debug_observer.on_context_built(ContextSnapshotRequest::from(context_builder, debug_iter, current_model)).await;
// ... business logic ...
self.core.debug_observer.on_phase_step(DebugPhase::Idle, None, output);
self.core.debug_observer.on_phase_step_done().await;
```

**The key changes**: the `if let Some(ctrl)` guards are deleted — the observer handles the no-op for
the `Production` variant internally; the 5 debug-specific methods on `loop_.rs` are deleted with
their logic moved into `DebugObserverImpl`; each hook call's semantics change from "check whether
debug exists and execute" to "notify the debug observer"; and the `await_debug_resume` logic moves
into the observer so the main loop only sees a boolean return.

#### 6. ContextBuilder keeps its patch interface but with clearer labeling

`apply_patches()` and the section accessors on ContextBuilder are **kept unchanged**, with these
adjustments: delete the "for debug patching" comments — these methods belong to ContextBuilder's
public API and should not be labeled debug-specific; rename `environment_override` to
`environment_patch`, shifting its semantics from "debug override" to "external patch" (the field may
have non-debug uses in the future, such as A/B test environment injection); and keep the section
accessors (`system_prompt()`, `tool_definitions()`, …) unchanged since they are effectively getters.

**Why not move everything into the observer?** ContextBuilder is a data object and `apply_patches`
is a data transformation. Having the observer hold a mutable reference to ContextBuilder to apply
patches is reasonable, but extracting the patch logic itself out of ContextBuilder brings no
benefit — it is just a field-level merge.

#### 7. Removing the debug variant from `SessionMessage`

**Before**:

```rust
pub enum SessionMessage {
    ChatMessage { ... },
    Stop { ... },
    EnableDebugMode(DebugHandles),  // debug-specific
    Close,
    // ...
}
```

**After**: delete the `EnableDebugMode` variant and use the `DebugObserverSlot` bypass injection
channel:

```rust
// session_handle.rs
pub struct SessionHandle {
    // ...
    debug_injection: DebugInjectionChannel,  // wraps Arc<Mutex<Option<DebugHandles>>>
}

impl DebugInjectionChannel {
    /// Inject a debug observer into a running session.
    /// Called by SessionManager when the Gateway pushes EnableDebugMode.
    pub fn inject(&self, handles: DebugHandles) { ... }
}
```

`SessionTask` no longer handles the `EnableDebugMode` message — debug injection happens at the
observer level through the `DebugInjectionChannel`.

#### 8. Converging the rewind logic

All three of `apply_debug_rewind`, `apply_debug_rewind_locked` and
`apply_debug_rewind_and_patches` move into `DebugObserverImpl`:

```rust
impl DebugObserverImpl {
    /// Apply any pending rewind, patches, and re-execute flag.
    /// Single lock acquisition for all three operations.
    async fn apply_rewind_and_patches(
        &self,
        session_id: &str,
        history: &mut HistoryManager,
        context_builder: &mut ContextBuilder,
    ) { ... }
}
```

The call site in `SessionTask` simplifies to:

```rust
self.core.debug_observer.apply_rewind_and_patches(
    &session_id, &mut agent_loop.session.history, context_builder
).await;
```

## File Change List

| File | Change | Description |
|---|---|---|
| `debug/mod.rs` | modified | add the `observer` and `observer_impl` submodule exports |
| `debug/observer.rs` | **new** | the `DebugObserver` trait + the `DebugObserverSlot` enum |
| `debug/observer_impl.rs` | **new** | `DebugObserverImpl` — the debug logic currently scattered everywhere converges here |
| `debug/controller.rs` | unchanged | internal state management, unaffected by the refactor |
| `debug/protocol.rs` | unchanged | protocol type definitions, unaffected |
| `debug/server.rs` | small change | the RPC handler call path is adjusted (from operating on ctrl directly to going through the observer) |
| `agent_core.rs` | **major change** | 6 Option fields → 1 `DebugObserverSlot`; 6 debug methods deleted |
| `loop_.rs` | **major change** | 5 debug methods deleted; 15+ call sites replaced with observer hooks |
| `context.rs` | small change | delete the "for debug patching" comments; `environment_override` → `environment_patch` |
| `session_task.rs` | **major change** | delete the `EnableDebugMode` message handling; the 3 rewind functions move into the observer; debug fields removed |
| `session_manager.rs` | medium change | `enable_debug_mode()` creates a `DebugObserverImpl` instead of `DebugHandles` |
| `session_handle.rs` | small change | `pending_debug_handles` → `DebugInjectionChannel` |
| `session_state.rs` | unchanged | comment references only, no substantive code |

## Module Dependencies After the Refactor

```
                        ┌─────────────────────────┐
                        │      AgentCore           │
                        │  debug_observer: Slot    │
                        └────────┬────────────────┘
                                 │
                    ┌────────────┴────────────┐
                    │                         │
            ┌───────▼───────┐        ┌───────▼───────┐
            │  Production   │        │  DevMode      │
            │  (no-op)      │        │  ObserverImpl │
            └───────────────┘        └───────┬───────┘
                                             │
                              ┌──────────────┼──────────────┐
                              │              │              │
                      ┌───────▼───┐  ┌───────▼───┐  ┌─────▼─────┐
                      │ Controller│  │ EventTx   │  │ Notify    │
                      │ (state)   │  │ (push)    │  │ (resume/  │
                      └───────────┘  └───────────┘  │  rewind)  │
                                                     └───────────┘
```

The main loop depends only on the `DebugObserverSlot` method signatures, not on the concrete types
`DebugController`, `DebugEventSender` or `Notify`.

## Implementation Strategy

Four steps, each independently compilable and testable:

**Step 1 — introduce the `DebugObserver` abstraction (non-breaking)**: add `debug/observer.rs` +
`debug/observer_impl.rs`; **add** the `debug_observer: DebugObserverSlot` field to `AgentCore`
(coexisting with the old fields); mark the old fields and methods `#[deprecated]`; **do not modify**
the call sites in `loop_.rs` / `session_task.rs`.

**Step 2 — migrate the main-loop hooks**: gradually replace the 15+ call sites in `loop_.rs` with
observer hooks. Migration order: `update_debug_phase` → `push_debug_step` +
`debug_auto_pause_if_stepping` → `capture_context_snapshot` → `await_debug_resume` → the guard
block at the start of an iteration. Run `cargo test` after each group to confirm no regression.

**Step 3 — migrate SessionTask and SessionManager**: move the `apply_debug_rewind*` function family
into `DebugObserverImpl`; delete `SessionMessage::EnableDebugMode` in favor of the
`DebugInjectionChannel`; change `SessionManager`'s `enable_debug_mode()` to create a
`DebugObserverImpl`.

**Step 4 — cleanup and deletion**: delete the 6 deprecated debug fields and methods from `AgentCore`;
delete `DebugHandles` (its responsibilities are taken over by `DebugObserverImpl` +
`DebugInjectionChannel`); clean up `context.rs` comments and rename the field; update the public
interface of `debug/mod.rs`.

## Consequences

### What gets better

| Dimension | Improvement |
|---|---|
| **Main-loop readability** | the 15+ `if let Some(ctrl)` guards disappear, replaced by semantically clear `observer.on_xxx()` calls |
| **AgentCore responsibility** | reduced from 6 fields + 6 methods to 1 field, returning to its "runtime core" positioning |
| **Debug module cohesion** | all debug logic (phase tracking, snapshots, pause/resume, rewind, patches) converges into `DebugObserverImpl` |
| **Testability** | `DebugObserver` can be mocked to test the main loop's various debug scenarios without starting a WebSocket server |
| **Zero-cost abstraction** | the Production variant is eliminated at compile time with no runtime overhead |
| **Future extensibility** | adding a debug hook only requires adding a method to the trait plus an impl — no changes to the business code's guard conditions |

### What gets worse (the cost)

| Dimension | Cost |
|---|---|
| **Indirection layer** | the main loop calls indirectly through the observer and cannot inline to the concrete implementation (though enum dispatch overhead is negligible) |
| **Observer method signatures** | trait methods must serve all call scenarios and may be "wider" than the current scattered code; some need `&mut self` or async, so the trait design requires care |
| **Migration risk** | each of the 4 migration steps needs full integration test coverage, especially for the rewind and pause/resume boundary cases |
| **DebugInjectionChannel** | the bypass injection mechanism still exists, just rewrapped — `Arc<Mutex<Option<DebugHandles>>>` becomes `DebugInjectionChannel`, essentially unchanged (though at least encapsulated) |

### What stays the same

- `debug/controller.rs` — internal state management, a pure data structure, unaffected
- `debug/protocol.rs` — protocol types, unaffected
- `debug/server.rs` — the WebSocket server needs minor RPC call path adjustments but is otherwise
  unchanged
- ContextBuilder's patch capability — preserved, only the caller changes from the loop operating
  directly to the observer proxying

## Rejected Alternatives

### A. Compile-time exclusion via a feature flag

```rust
#[cfg(feature = "debug")]
{
    self.update_debug_phase(DebugPhase::BudgetCheck).await;
}
```

**Rejected because**: a production build cannot include debug functionality, but ACowork's DevMode
is a runtime switch (the Gateway pushes `EnableDebugMode`), not a compile-time choice; feature flags
do not support the "runtime injection" scenario; and `cfg` conditional compilation means the code
of both modes can never be tested simultaneously.

### B. Using a macro to eliminate boilerplate

```rust
debug_hook!(self, on_phase_enter, DebugPhase::BudgetCheck);
```

**Rejected because**: a macro merely hides the guard condition without changing the essence of
debug logic being scattered everywhere; the code after macro expansion is still coupled into the
business file; and readability drops — developers must understand macro expansion to understand the
behavior.

### C. Fully event-driven (an event bus)

Base the debug functionality entirely on an event bus: the main loop emits events and the debug
module subscribes.

**Rejected because**: the main loop's pause/resume is **synchronous blocking semantics** — after
emitting an event it must wait for the debug resume before continuing, and the event bus's
asynchronous nature conflicts with this synchronous need; an event bus introduces timing
nondeterminism (event ordering, backpressure) while debug requires determinism; and it increases
the complexity of global infrastructure.

### D. Dynamic dispatch (`Option<Box<dyn DebugObserver>>`)

**Rejected because**: vtable indirection has measurable overhead on hot paths (though small);
`Box<dyn>` means a heap allocation, which is undesirable for an object accessed every iteration;
and enum dispatch is no worse than dynamic dispatch in any dimension while offering better compiler
optimization opportunities.

## References

- Current code: `core/acowork-runtime/src/agent/loop_.rs` (4189 lines), `agent_core.rs`,
  `context.rs`, `session_task.rs`
- Design doc: `docs/design/zh/10-debug-protocol.md`
- Inspirations: the CDP Session model from Chrome DevTools Protocol, the Observer pattern from LLDB
