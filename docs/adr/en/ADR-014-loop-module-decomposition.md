# ADR-014: Decomposing the AgentLoop Main Loop — From God Object to Responsibility Modules

> **Chinese source of truth**: [ADR-014](../zh/ADR-014-loop-module-decomposition.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented (8/8 phases complete)

## Date

2026-06-05

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

`agent/loop_.rs` (3908 lines → 2024 lines) and all of its callers

---

## Context

`loop_.rs` is the largest single file in ACowork Runtime at 3908 lines (2635 production + 1273
test). It contains all of the AgentLoop's business logic, but 8 orthogonal concerns are mixed
together with no physical boundary isolating them.

### Problem 1: the God Method `execute_single_iteration` (742 lines)

A single method accounting for **28%** of the file's production code, containing 25+ inline
blocks that mix completely different responsibilities: budget check, context build, LLM call, tool
dispatch, loop detection, JSONL persist, debug hooks.

**The core symptom**: you cannot modify any single block without understanding the other 24. Each
block's variable dependency graph is interwoven with its neighbours — `context_builder`,
`current_model`, `response`, `self.session.history` and similar variables flow between blocks with
no clear interface boundary.

### Problem 2: 5 places of duplicated code

| # | Duplication pattern | Occurrences | Total redundant lines |
|---|---|---|---|
| D1 | InboundMessage → ChatMessage injection | 3 (drain_inbound deferred/live + run_inner iteration pause) | ~60 |
| D2 | Think block persistence | 2 (text response path + tool calls path) | ~12 |
| D3 | Stop handling | 2 (pre-tool stop + post-tool stop) | ~30 |
| D4 | `await_approval_decision` vs `await_question_answer` | 2 (the `select!` loop structures are isomorphic) | ~90 |
| D5 | The `APPROVAL_TIMEOUT_SECS` constant | 2 hardcoded 300 | — |

**~192 lines of redundancy in total**, and every duplicate is a breeding ground for "change one
place and forget the other" bugs.

### Problem 3: mixed responsibilities cause cognitive overload

`loop_.rs` mixes 8 orthogonal concerns:

| Concern | Methods | Lines (est.) | Representative methods |
|---|---|---|---|
| Context management | 7 | ~310 | `compact_history_if_needed`(171), `resolve_distill_model`(50), `trim_history_to_budget`(20) |
| Approval subsystem | 5 | ~167 | `await_approval_decision`(103), `handle_approval_request`(37), `ApprovalHandle`(20) |
| User interaction | 3 | ~227 | `handle_ask_user_question`(59), `handle_todo_write`(80), `await_question_answer`(88) |
| Inbound messages | 3 | ~208 | `drain_inbound_queue`(124), `poll_stop`(40), `apply_user_op`(44) |
| Session lifecycle | 7 | ~143 | `close_session_inner`(93), the constructor(52), `transition_status`(24) |
| Memory system | 3 | ~98 | `retrieve_and_inject_memories`(76), `init_memory_store`(3), `write_document_entries`(19) |
| Debug hooks | call sites | ~90 | already migrated to the observer; the call sites remain here |
| Core orchestration | 4 | ~246 | `run_inner`(194), `run/replay`(6), `execute_tool_by_name`(20), `execute_single_iteration`(skeleton) |

A developer trying to understand "the approval timeout logic" has to find a 103-line method inside
a 3908-line file and then understand its dependency relationships with the other methods — **the
cognitive cost is proportional to the total file length, not to the target method's length.**

### Problem 4: existing split precedents prove the pattern works

`loop_llm.rs` (405 lines) and `loop_tools.rs` (572 lines) have already been successfully extracted
from `loop_.rs` using the split-file `impl AgentLoop` pattern. Both compile independently, are
tested independently, and introduced no circular dependencies or performance regression. This proves
**decomposing `impl AgentLoop` by responsibility is safe and effective.**

## Decision

Split `loop_.rs` by 6 orthogonal concerns into independent modules, and refactor
`execute_single_iteration` from a 742-line God Method into an ~80-line orchestration skeleton plus 13
sub-methods. Use a phased implementation strategy where each phase is independently compilable and
testable.

### Core Principles

1. **Continue the split-file `impl AgentLoop` pattern** — consistent with `loop_llm.rs` /
   `loop_tools.rs`, introducing no new architectural pattern
2. **Extract methods first, move files second** — after each extraction compile and test to ensure
   no regression, then move to a new file
3. **Duplication over abstraction** — tolerate temporary duplication during the split phase, and
   eliminate it once all module boundaries are stable
4. **Phased delivery** — each phase is an independent PR that can be reviewed and rolled back
   separately

### The Module Split

#### Phase 1: `loop_context.rs` — context management (highest priority)

**Rationale**: the most lines (~310), contains the largest single method
`compact_history_if_needed` (171 lines), and context management is the most deeply coupled concern
in the main loop — 6 of the inline blocks in `execute_single_iteration` belong to it.

| Method | Source | Lines |
|---|---|---|
| `compact_history_if_needed` | loop_.rs | 171 |
| `resolve_distill_model` | loop_.rs | 50 |
| `trim_history_to_budget` | loop_.rs | 20 |
| `context_trim_budget` | loop_.rs | 3 |
| `update_provider` | loop_.rs | 11 |
| `update_gateway_model_capabilities` | loop_.rs | 3 |
| `update_max_output_tokens_limit` | loop_.rs | 3 |
| `apply_runtime_config` | loop_.rs | 10 |

**Sub-methods extracted from `execute_single_iteration`**:

| New method | Source block | Lines | Description |
|---|---|---|---|
| `check_budget_and_warn()` | B3 | 25 | the budget pre-check + warning |
| `build_chat_request()` | B5+B7 | 22 | build the ChatRequest + MCP tool merge |
| `check_context_overflow_and_trim()` | B6 | 38 | the context-overflow circuit breaker |
| `process_llm_response_usage()` | B9 | 105 | post-LLM usage reporting + budget update |
| `pre_trim_for_tool_results()` | B19 | 23 | pre-trim before tool results |

#### Phase 2: `loop_approval.rs` — the approval subsystem

**Rationale**: eliminates duplication #4 (`await_approval_decision` and `await_question_answer` are
isomorphic), and the approval logic is entirely orthogonal to the main loop — the main loop only
touches `ApprovalHandle` in `execute_tools_parallel` and does not need to know its internals.

| Method / type | Source | Lines |
|---|---|---|
| `ApprovalHandle` | loop_.rs | 20 |
| `ApprovalDecision` | loop_.rs (type) | — |
| `await_approval_decision` | loop_.rs | 103 |
| `await_question_answer` | loop_.rs | 88 |
| `handle_approval_request` | loop_.rs | 37 |
| `send_tool_approval_needed` | loop_.rs | 13 |

**Deduplication plan**: extract a generic waiter `InboundWaiter` encapsulating the `tokio::select!`
loop + deferred cache + timeout + Stop-signal handling:

```rust
/// Generic waiter for specific inbound messages.
/// Encapsulates the common select! loop shared by approval and question flows.
struct InboundWaiter<'a> {
    inbound_rx: &'a mut mpsc::Receiver<InboundMessage>,
    approval_rx: &'a mut mpsc::Receiver<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    deferred: &'a mut Vec<InboundMessage>,
    request_id: String,
    timeout_secs: u64,
}

impl<'a> InboundWaiter<'a> {
    /// Wait for an inbound message matching the predicate.
    /// Returns the matched message, or None on timeout/stop.
    async fn wait_for<F, T>(
        &mut self,
        match_fn: F,
        on_stop: impl FnOnce() -> T,
        on_timeout: impl FnOnce() -> T,
    ) -> Option<T> { ... }
}
```

#### Phase 3: `loop_inbound.rs` — inbound message handling

**Rationale**: eliminates duplication #1 (3 places of message injection); inbound messages are the
main loop's interface to the outside world and deserve their own module.

| Method | Source | Lines |
|---|---|---|
| `drain_inbound_queue` | loop_.rs | 124 |
| `poll_stop` | loop_.rs | 40 |
| `apply_user_op` | loop_.rs | 44 |

**Deduplication plan**: extract the `inject_inbound_into_history()` helper to eliminate the 3
places of message-injection duplication:

```rust
/// Convert an InboundMessage into ChatMessage(s) and append to history.
/// Used by drain_inbound_queue, run_inner iteration-limit pause, and poll_stop.
fn inject_inbound_into_history(msg: InboundMessage, history: &mut HistoryManager) {
    match msg {
        InboundMessage::UserMessage { content, .. } => {
            history.push_message(ChatMessage::user(&content));
        }
        InboundMessage::SystemNotification { content, .. } => {
            history.push_message(ChatMessage {
                role: Role::User,
                name: Some("system".into()),
                content,
                ..Default::default()
            });
        }
        InboundMessage::IntentMessage { from_agent, action, params, .. } => {
            history.push_message(ChatMessage::user(&format!("[Intent from {from_agent}: {action}] {params}")));
        }
        _ => { /* other message types are not injected into history */ }
    }
}
```

#### Phase 4: `loop_interaction.rs` — user interaction

**Rationale**: the interception and interaction logic for the 3 "special tools" (ask_user_question,
todo_write, ask_question) is unrelated to the main loop — it is an independent user-interaction
sub-protocol.

| Method | Source | Lines |
|---|---|---|
| `handle_ask_user_question` | loop_.rs | 59 |
| `handle_todo_write` | loop_.rs | 80 |
| `await_question_answer` → moved into `InboundWaiter` in Phase 2 | — | — |

#### Phase 5: `loop_session.rs` — session lifecycle

**Rationale**: session creation/closing/distillation is lifecycle management independent of the
main loop's orchestration.

| Method / type | Source | Lines |
|---|---|---|
| `new` / `new_with_observer` / `from_core_and_session` | loop_.rs | 52 |
| `transition_status` | loop_.rs | 24 |
| `close_session_inner` | loop_.rs | 93 |
| `close_session_with_distillation` | loop_.rs | 3 |
| `current_session_id` | loop_.rs | 3 |
| `update_session_title` | loop_.rs | 3 |
| `update_session_workspace_id` | loop_.rs | 5 |
| `extract_think_block` / `strip_think_block` / `build_think_metadata` | loop_.rs (free functions) | 32 |

**Deduplication plan**: extract `persist_think_block()` to eliminate duplication #2 (2 places of
think persistence).

#### Phase 6: `loop_memory.rs` — the memory system

**Rationale**: memory retrieval/injection is tightly coupled to Grafeo and entirely unrelated to
the main loop.

| Method | Source | Lines |
|---|---|---|
| `retrieve_and_inject_memories` | loop_.rs | 76 |
| `init_memory_store` | loop_.rs | 3 |
| `write_document_entries` | loop_.rs | 19 |

### The `loop_.rs` Skeleton After the Split

After the split `loop_.rs` retains only the core orchestration logic:

```rust
// loop_.rs — core orchestration (target ~800 lines, tests included)

mod loop_context;   // Phase 1
mod loop_approval;  // Phase 2
mod loop_inbound;   // Phase 3
mod loop_interaction; // Phase 4
mod loop_session;   // Phase 5
mod loop_memory;    // Phase 6
mod loop_llm;       // already exists
mod loop_tools;     // already exists

impl AgentLoop {
    // ── Core orchestration ──
    pub async fn run(&mut self) -> Result<LoopResult> { ... }
    pub async fn replay(&mut self) -> Result<LoopResult> { ... }
    async fn run_inner(&mut self, replay: bool) -> Result<LoopResult> { ... }
    pub(crate) async fn execute_single_iteration(&mut self) -> Result<IterationResult> {
        // ~80-line orchestration skeleton calling each submodule's methods
    }

    // ── Accessors ──
    pub fn history(&self) -> &HistoryManager { ... }
    pub fn manifest(&self) -> &AgentManifest { ... }
    pub fn history_mut(&mut self) -> &mut HistoryManager { ... }
}
```

After the refactor `execute_single_iteration` is about 80 lines and contains only:

```
① debug observer hooks + resume
② check_budget_and_warn()
③ build_chat_request()
④ call_llm_streaming()         ← loop_llm.rs
⑤ process_llm_response_usage()
⑥ text response → handle_text_response() → return
⑦ deduplicate + pre_check_loop_detection()
⑧ tool dispatch → execute_tools_parallel() ← loop_tools.rs
⑨ merge results + post_check_loop_detection()
⑩ pre_trim_for_tool_results()
⑪ debug phase completion
```

## File Change List

**Phase 1 — `loop_context.rs`**: `loop_context.rs` **new** (8 methods + 5 sub-methods extracted
from `execute_single_iteration`); `loop_.rs` **major change** (delete the 8 methods + replace the 5
inline blocks in `execute_single_iteration` with method calls); `agent/mod.rs` small change (add
`mod loop_context`).

**Phase 2 — `loop_approval.rs`**: `loop_approval.rs` **new** (ApprovalHandle + 4 methods +
InboundWaiter dedup); `loop_.rs` **major change** (delete the 6 approval methods, replace with
submodule calls); `loop_tools.rs` small change (adjust the import path for `ApprovalHandle`);
`agent/mod.rs` small change (add `mod loop_approval`).

**Phase 3 — `loop_inbound.rs`**: `loop_inbound.rs` **new** (3 methods + the
`inject_inbound_into_history` helper); `loop_.rs` **medium change** (delete the 3 methods, replace
the message injection in `run_inner` with the helper); `agent/mod.rs` small change.

**Phase 4 — `loop_interaction.rs`**: `loop_interaction.rs` **new** (2 methods — `await_question_answer`
already moved into `InboundWaiter` in Phase 2); `loop_.rs` **medium change** (delete the 2
interaction methods, extract the interception logic from tool dispatch); `agent/mod.rs` small
change.

**Phase 5 — `loop_session.rs`**: `loop_session.rs` **new** (the constructor + lifecycle methods +
free functions); `loop_.rs` **medium change** (delete the constructor and lifecycle methods);
`agent/mod.rs` small change.

**Phase 6 — `loop_memory.rs`**: `loop_memory.rs` **new** (3 memory methods); `loop_.rs` **small
change** (delete the 3 memory methods); `agent/mod.rs` small change.

**Phase 7 — dedup + core extraction (`execute_single_iteration` skeletoning, part 1)**:
`loop_session.rs` **medium change** (+`persist_think_to_conversation()` for D2 dedup +
`handle_text_response()`); `loop_inbound.rs` **medium change** (+`handle_stopped()` for D3 dedup);
`loop_.rs` **major change** (+`await_debug_resume()`; replace the inline text-response / stopped
code).

**Phase 8 — extracting the tool pipeline (`execute_single_iteration` skeletoning, part 2)**:
`loop_tools.rs` **major change** (+`prepare_tool_calls()` +`pre_check_loop_detection()` +
`dispatch_and_merge_tools()` +`persist_and_emit_tool_results()` +`post_check_loop_detection()`);
`loop_.rs` **major change** (replace the 5 segments of inline tool-pipeline code with sub-method
calls).

## Expected Results After the Refactor

| Metric | Before | After (actual) |
|---|---|---|
| `loop_.rs` total lines | 3908 | 2024 |
| `loop_.rs` production code | 2635 | ~1300 (tests included) |
| `execute_single_iteration` | 742 lines | 106 lines |
| File count | 3 (loop_ + loop_llm + loop_tools) | 9 (+6 new modules) |
| Code duplication | 5 places / ~192 lines | D1+D2+D3+D5 eliminated (~142 lines), D4 deferred (see Risk 4) |
| Largest single method | 742 lines | ~171 lines (`compact_history_if_needed`) |

### Estimated Lines per Module

```
loop_.rs          ~400  (core orchestration)
loop_context.rs   ~310  (context management)
loop_llm.rs        405  (LLM calls, already exists)
loop_tools.rs      572  (tool execution, already exists)
loop_approval.rs   ~170  (approval subsystem)
loop_inbound.rs    ~210  (inbound messages)
loop_interaction.rs ~140 (user interaction)
loop_session.rs    ~210  (session lifecycle)
loop_memory.rs      ~100  (memory system)
────────────────────────
total             ~2517  (vs the current loop_.rs at 2635 lines)
```

The total drops slightly (dedup eliminates ~192 lines and skeletoning `execute_single_iteration`
saves ~662 lines, while new method signatures / parameters / return values add ~100 lines of
overhead).

## Risks and Mitigations

**Risk 1 — parameter explosion in the sub-methods.** When extracting sub-methods from
`execute_single_iteration`, the inline blocks access many outer variables
(`self.session.history`, `context_builder`, `current_model`, `response`, …) which must be passed
as parameters. Mitigation: introduce a lightweight `IterationContext` struct bundling the
frequently co-used variables:

```rust
struct IterationContext<'a> {
    history: &'a mut HistoryManager,
    budget_guard: &'a mut BudgetGuard,
    current_model: &'a str,
    conversation: &'a ConversationSession,
}
```

Only introduce the context struct when there are ≥ 4 parameters; below that, pass them directly.
**Not introduced in Phase 1** — decide based on the actual parameter count after Phase 1.

**Risk 2 — split-file `impl AgentLoop` has no compile-time isolation.** Every `loop_*.rs` file can
access all of `self`'s fields and call the other files' methods, so there is no enforced boundary
after the split. Mitigation: accept this limitation — Rust's split-file `impl` pattern is a
convention, not enforcement; enforce module boundaries through code review and documentation; if
stronger isolation is needed later, introduce mediating structs (e.g. `LoopContext`,
`LoopSession`), which belongs in a follow-up ADR.

**Risk 3 — running 6 phases simultaneously = high regression risk.** Every method move can
introduce compile errors or behavior changes. Mitigation: **execute strictly in phase order**, with
each phase an independent PR; within each phase follow the "extract the method → compile and test →
move the file → compile and test" rhythm; after each phase run the full test suite (`cargo test`)
to confirm no regression. Phases 1 and 2 are the highest priority (most lines + most duplication),
while Phases 5–6 can be done later.

**Risk 4 — generalizing `InboundWaiter` may over-abstract.** Although
`await_approval_decision` and `await_question_answer` are structurally isomorphic, their return
types and error handling differ, and generalizing may increase rather than reduce comprehension
cost. Mitigation: in Phase 2 first do the simple extraction (move both to the same file) without
hurrying to generalize; if after co-location the two methods really can share 80% of the code, do
`InboundWaiter`; if the differences outweigh the commonalities, keep the two methods but in the
same file — which is already an improvement.

## Consequences

### What gets better

| Dimension | Improvement |
|---|---|
| **Maintainability** | changing context logic only requires understanding `loop_context.rs` (~310 lines), not 3908 lines |
| **Testability** | each module can have its own `mod tests`, making the tests more focused |
| **Duplication** | 5 places / ~192 lines → 0 |
| **Cognitive load** | a newcomer understanding AgentLoop's core orchestration path only needs to read ~400 lines |
| **God Method** | `execute_single_iteration` goes from 742 lines to ~80, with each sub-step clearly named |
| **Review efficiency** | PR changes concentrate in a single module, so the reviewer does not need to understand unrelated concerns |

### What gets worse (the cost)

| Dimension | Cost |
|---|---|
| **More files** | 3 → 9 files; you have to remember which file a method is in |
| **Method signatures exposed** | some methods must be promoted from `private` to `pub(crate)` for cross-file calls |
| **Weak compile-time isolation** | split-file `impl AgentLoop` has no compile-time enforced boundary and relies on convention |
| **Git blame history** | after methods move, `git blame` has to follow file renames |
| **Transition period** | during the 6 phases some methods may have moved while callers remain in the old file |

### What stays the same

- `loop_llm.rs` and `loop_tools.rs` — already exist, unaffected
- Data structures such as `AgentCore`, `SessionState`, `HistoryManager` — unchanged
- `DebugObserver` and its call sites — unchanged (ADR-013 already completed)
- The external API (`AgentLoop::new`, `run`, `replay`) — signatures unchanged

## Rejected Alternatives

### A. Builder pattern to replace `execute_single_iteration`

```rust
IterationBuilder::new(&mut self)
    .check_budget()
    .build_context()
    .call_llm()
    .dispatch_tools()
    .execute()
```

**Rejected because**: the AgentLoop's iteration is not a composable pipeline — the steps have
complex conditional branches (a text response exits directly, loop detection may interrupt, a stop
signal can insert at any time); the Builder pattern implies steps are optional and reorderable, but
AgentLoop's steps are in fixed order; and it adds an indirection layer without reducing complexity.

### B. State machine pattern

```rust
enum IterationState {
    BudgetCheck, BuildContext, LlmCall, ParseResponse,
    ToolDispatch, ToolResult, LoopDetection, Done
}
```

**Rejected because**: a state machine suits scenarios with complex transitions and rollback, but
AgentLoop's iteration is a linear flow with no rollback; each state would need to persist
intermediate variables (`response`, `tool_results`, …), requiring an additional state-holding
struct; and it adds a lot of boilerplate (`match state { ... }`, the `IterationState` enum
definition) without solving the responsibility-mixing problem.

### C. Introducing a `LoopContext` mediating struct (strong isolation)

**Rejected because**: strong isolation is not needed at this stage — the split-file
`impl AgentLoop` pattern is already sufficient; introducing a mediating struct means every method
must change from `&mut self` to `&mut LoopContext`, a change surface that is too large; and if
strong isolation is genuinely needed later it can be introduced as a follow-up ADR without
affecting the current file split.

### D. Splitting everything at once rather than in phases

**Rejected because**: moving ~2500 lines of code at once carries an extremely high regression risk;
if a module's interface design turns out to be wrong, rollback is difficult; and phasing allows
verifying design decisions at each stage and adjusting promptly.

## References

- Current code: `core/acowork-runtime/src/agent/loop_.rs` (3908 lines)
- Existing split precedents: `loop_llm.rs` (405 lines), `loop_tools.rs` (572 lines)
- Preceding ADR: ADR-013 (Debug Observer Pipeline)
- Inspirations: Martin Fowler's *Refactoring* — "Extract Method" + "Decompose Conditional"
