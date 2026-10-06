# ADR-045: Tool Execution Progress Heartbeats and Single-Tool Cancellation

> **Chinese source of truth**: [ADR-045](../zh/ADR-045-tool-progress-and-cancel.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented

## Date

2026-08-03

## Decision Makers

大鱼 (Dayu)

## Predecessors

- ADR-014 (AgentLoop main-loop module decomposition)
- ADR-021 (dropping the data-plane streaming push in favour of frontend HTTP pull)
- ADR-033 (MQTT replacing gRPC + WebSocket — the control channel unified as MQTT)
- ADR-034 (MQTT / HTTP responsibility boundary)
- ADR-044 (CancelHandle unification of the Stop signal path)

## Trigger

- A single shell tool execution can take 1–10 minutes (`cargo build`, `npm install`, large file
  downloads, long-running watch commands), exceeding the user's patience threshold
- The current UX (`apps/acowork-desktop/src/components/chat/ExploreBlock.tsx:614-616`): during tool
  execution the frontend only shows an `animate-pulse rounded-full` grey dot — **the user cannot
  see elapsed time, remaining time, nor cancel midway**
- Research fact ([`tools/builtin/shell.rs:276-305`](../../../core/acowork-runtime/src/tools/builtin/shell.rs)):
  `wait_with_output()` is an OS-level block that emits no MQTT/HTTP events at all
- Research fact ([`loop_tools.rs:133-148`](../../../core/acowork-runtime/src/agent/loop_tools.rs)): the
  single-tool timeout is `tool_timeout_ms` (default 10 min), but it is a passive wait until expiry
- The user's explicit ask: "the frontend is very unfriendly, it just sits there waiting"

---

## 1. Goals and Non-Goals

### 1.1 Goals

1. **Observable**: during long tool executions the frontend can see "elapsed / total timeout" plus
   a progress bar
2. **Interruptible**: the user can abort the current tool mid-execution, letting the LLM see a
   "cancelled by the user" result and continue reasoning; the iteration as a whole is unaffected
3. **Protocol consistency**: reuse the existing `UserOp` + `mqtt_publish_control` + `CancelHandle`
   mechanisms rather than inventing a new IPC path
4. **Progressive UX (new in §3.2 / §4)**: heartbeats are delayed — tools finishing within 5s keep the
   original UX (just a small grey dot), and **only tools running past 5s escalate to "timer +
   progress bar + cancel button"**. Short commands are undisturbed; long commands get full control

### 1.2 Non-Goals

- ❌ No streaming of shell stdout/stderr back to the frontend (belongs to ADR-046 / a later
  topic; out of scope here)
- ❌ No "pause / resume" for an individual tool (keep cancel-only semantics to minimize complexity)
- ❌ No change to the `tool_timeout_ms` default (keep 10 min), but the heartbeat event carries that
  value so the frontend can display it correctly
- ❌ No new IPC channel (no new HTTP endpoint; cancel goes over MQTT)
- ❌ No aggressive "show the full panel from the moment the tool starts" mode (it would ruin the
  simplicity of short commands)

## 2. Current State (facts)

### 2.1 Tool execution and timeout (existing)

```
┌──────────────────────────────────────────────────────────────────┐
│ loop_tools.rs:85-152                                              │
│   tool_timeout = Duration::from_millis(tool_timeout_ms)         │
│   for tc in tool_calls {                                          │
│       tokio::spawn(execute_single_tool(...))                     │
│   }                                                               │
│   for each future:                                                │
│       match tokio::time::timeout(                                  │
│           tool_timeout, await future                              │
│       ) { ... }                                                   │
└──────────────────────────────────────────────────────────────────┘
```

- [`shell.rs:260-305`](../../../core/acowork-runtime/src/tools/builtin/shell.rs): calls
  `wait_with_output()` inside `tokio::task::spawn_blocking` — **a complete block, emitting no events
  at all**
- [`timeout_config.rs`](../../../core/acowork-core/src/timeout_config.rs): `tool_timeout_ms = 600_000`
  (10 min); `iteration_timeout_ms = 900_000` (15 min)

### 2.2 The existing Stop / Pause / Resume handling pattern (reusable)

```
[1] Desktop ChatPanel.tsx
        invoke("mqtt_publish_control", { command: "stop", payload: {...} })
        ↓
[2] Tauri mqtt_publish_control → Gateway MQTT broker
        publish on acowork/agents/{id}/control/{sid}
        ↓
[3] Runtime gateway_loop.rs:66  mqtt_dispatch_tx.send(...)
        parse_control_payload → ControlAction
        ↓ control_action_to_inbound
[4] InboundMessage::Stop { reason }  ─┐
   InboundMessage::UserOperation(op) ─┤  go through the session_task inbox
                                      ↓
[5] AgentLoop self.inbound_rx
        ↓ poll_control() (a non-blocking try_recv at every checkpoint)
[6] ControlDecision::Stop  → the internal flow returns
        ↓ ADR-044 CancelHandle  +  the urgent_stop Notify  (loop_tools.rs:218/271)
[7] the select! hits → handle.abort() / kill()
```

**Four key existing facilities that must be reused**:

1. the `UserOp` enum at `core/acowork-runtime/src/agent/inbound.rs:44-64` (add a `CancelTool`
   variant)
2. the `control_action_to_inbound` single mapper at
   `core/acowork-runtime/src/startup/gateway_loop.rs:145+` (add a `CancelTool` route)
3. the `ControlAction` enum in `core/acowork-core/src/mqtt/control_handler.rs` (add a `CancelTool`
   variant)
4. the urgent_stop Notify pattern inside the `tokio::select!` at
   `core/acowork-runtime/src/agent/loop_tools.rs:218/271` (add a tool-level cancel trigger point)

### 2.3 The current UX (`ExploreBlock.tsx:609-617`)

```tsx
{isSuccess ? <Check /> :
 isError   ? <X /> :
 isPendingResult ? <span className="... animate-pulse rounded-full bg-zinc-300" /> :
 null}
```

- Between "tool_call persisted" and "tool_result persisted" the UI has **no time display, no
  countdown and no cancel button**
- ADR-021 removed the streaming data events, so the frontend must overlay heartbeats on top of the
  HTTP-pulled JSONL

## 3. Decision

### Decision A: progress heartbeat (one emission every N=5 seconds per tool execution)

#### 3.1 The new event type

Add to the `ChunkEvent` enum at `core/acowork-runtime/src/agent/loop_.rs:69-` (note: ADR-021
deleted the data events, but this event is a **pure control-plane signal** carrying no data payload
for the frontend to render UI with — it only triggers a "re-render / update the timer"):

```rust
/// ADR-045: Tool execution progress heartbeat.
/// Pure control-plane signal — carries NO tool result data.
/// Frontend uses it to refresh a timer/countdown display only.
ToolProgress {
    session_id: String,
    tool_call_id: String,
    elapsed_ms: u64,    // total time since the tool was spawned
    timeout_ms: u64,    // = tool_timeout_ms (the frontend computes the percentage)
},
```

> **Strict boundary**: this event must not carry any stdout/stderr, error message or `tool_result`
> field. After receiving it the frontend should only use it to refresh the already-displayed
> "Running… Xm Ys" label; it must not use it to replace an already-completed `tool_result`.

#### 3.2 Where the heartbeat is emitted

In the outer layer of `execute_single_tool` in
`core/acowork-runtime/src/agent/loop_tools.rs` (between lines 70 and 152), using `tokio::select!`
— **without touching any code in the inner tool implementation**:

```rust
// Pseudocode (the real code is in the follow-up PR)
let tool_start = Instant::now();
let progress_handle = tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.tick().await; // skip the immediate first tick
    loop {
        interval.tick().await;
        let event = ChunkEvent::ToolProgress {
            session_id: session_id.clone(),
            tool_call_id: tc.id.clone(),
            elapsed_ms: tool_start.elapsed().as_millis() as u64,
            timeout_ms: tool_timeout_ms,
        };
        if chunk_tx.try_send(event).is_err() { break; }
        if tool_start.elapsed() > Duration::from_millis(tool_timeout_ms) { break; }
    }
});

let result = match tokio::time::timeout(tool_timeout, future).await { ... };

progress_handle.abort();  // stop the heartbeat goroutine immediately on completion
```

- The heartbeat task uses a non-blocking `try_send` → it does not affect tool execution
- `abort()` stops it immediately on completion
- No IPC protocol field is added — it is just one more `ChunkEvent` variant reusing the existing MQTT
  channel

**The first heartbeat is delayed (the 5s experience valve)**:

```rust
let mut interval = tokio::time::interval(Duration::from_secs(5));
interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
// Key: the first tick is immediate, but we need 5s before the first send — so consume
// the immediate tick first
interval.tick().await;  // skip the immediate tick (the one at Instant::now)
loop {
    interval.tick().await;
    // ... try_send ChunkEvent::ToolProgress
}
```

**Why 5s (design basis)**:

- **short commands are not disturbed**: common tools (`ls`, `grep`, `cat`, single-file reads,
  web_fetch, …) almost always finish within 5s. If the UI upgraded at 1s, every command would
  "flash" a full panel and then disappear, which is a UX regression
- **long commands are not delayed**: 5s is where users start to get anxious. From that point the UI
  shows "elapsed 5s / 10m 0s + progress bar 0.8%" plus a cancel button — the user immediately
  perceives "this tool is stuck"
- **zero protocol cost**: no "UI escalation trigger event" is needed — **the first heartbeat is
  itself the signal**. The frontend simply upgrades when
  `progressByToolCallId.has(id) === true`, and **network latency is negligible** (the heartbeat
  fires at 5s, reaches the frontend at 5.0x s, and the UI upgrade also happens at 5.0x s — the
  precision is acceptable)
- **the cancel button's usable window**: the tool timeout is 10 min. Escalating the UI at 5s gives
  the user a 9m55s window to click cancel

> This is a UX-only decision with **zero impact on the protocol / event payload**. The backend only
> needs to ensure the first heartbeat is not emitted when the tool starts.

#### 3.3 The heartbeat's downstream path

- `ChunkEvent::ToolProgress` → `try_send_chunk` → `MqttChunkPublisher` → the existing
  `acowork/agents/{id}/chunks/{sid}` topic → Gateway → Desktop subscribes
- Delivered in parallel with the existing `RecordComplete`, **not blocking the main flow**
- A heartbeat failure (the MQTT broker is down) → `try_send` fails → silently swallowed, **never
  letting the heartbeat affect the tool execution itself**
- **The frontend treats "receiving the first heartbeat" as the UI escalation signal** — short
  commands (finishing within 5s) never trigger a heartbeat so the UX is unchanged; long commands
  escalate from a `pendingToolsCount` grey dot to the full panel (see §4)

### Decision B: cancelling a single tool

#### 3.4 The new UserOp / InboundMessage / ControlAction variants

| File | What to add |
|---|---|
| `core/acowork-runtime/src/agent/inbound.rs:44-64` | `UserOp::CancelTool { tool_call_id: String }` |
| `core/acowork-runtime/src/agent/inbound.rs:67-` | `InboundMessage::UserOperation(UserOp)` (already exists, unchanged), carrying the above UserOp |
| `core/acowork-core/src/mqtt/control_handler.rs` | `ControlAction::CancelTool { session_id, tool_call_id }` |
| `core/acowork-runtime/src/startup/gateway_loop.rs:145-` | a new match branch in `control_action_to_inbound`: `Some((sid, InboundMessage::UserOperation(UserOp::CancelTool { tool_call_id })))` |
| Desktop Tauri `mqtt_publish_control` (already exists) | **no Rust backend change needed** — the frontend only has to send `command: "cancel_tool"` (the command name is parsed by `parse_control_payload`) |

#### 3.5 The cancellation path inside the Runtime (the core)

Wrap the outer layer of `execute_single_tool` in `loop_tools.rs` with a per-tool cancel token:

```rust
// Pseudocode
let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
self.pending_tool_cancels.borrow_mut().insert(tc.id.clone(), cancel_tx);

let result = tokio::select! {
    res = tokio::time::timeout(tool_timeout, execute_inner(...)) => res,
    _ = cancel_rx.wait_for(|v| *v) => {
        // kill the inner process (see decision D)
        Err(ToolError::CancelledByUser)
    }
};

self.pending_tool_cancels.borrow_mut().remove(&tc.id);
```

- add the new field `pending_tool_cancels: Rc<RefCell<HashMap<String, watch::Sender<bool>>>>` to
  `AgentLoop`
- `apply_user_op(&UserOp::CancelTool)` sets the corresponding `cancel_tx.send(true)`
- using a watcher instead of a mutex means zero contention
- after cancel, the whole tool flow returns `Err(ToolError::CancelledByUser)`, and **the LLM receives
  "Tool X was cancelled by user after Ys" and continues reasoning**

#### 3.6 The tool result after cancellation

The `tool_result` JSON looks like:

```json
{
  "success": false,
  "error": "Cancelled by user after 12s",
  "exit_code": null,
  "stdout": "<output read so far>",
  "stderr": ""
}
```

- Seeing the error, the LLM can choose: switch tools, change parameters, or ask the user
- **Never silently dropped**: even a cancelled tool is written to the `tool_result` and history, so
  the LLM sees it

### Decision D: cancellation implementation details (the most important engineering issue)

#### 3.7 Process cleanup for the shell tool (already present today)

The `ProcessGuard` at `shell.rs:296-305` calls `child.kill()` + `child.wait()` on Drop — **this
mechanism already handles mid-flight cancellation correctly**, as long as the outer future is
`Drop`ped when `cancel_rx` fires. `tokio::select!`'s cancellation semantics are exactly this: a
branch hits → the outer future is dropped → `ProcessGuard` is dropped → the child process is
killed. **This path requires no change to shell.rs.**

#### 3.8 WASM tools (WASI)

WASM tools run under wasmtime, which **do not currently support runtime cancellation** (once a
wasmtime instance enters a host call it cannot be interrupted). This ADR keeps that limitation:

- the cancel event arrives first but only takes effect when the tool returns naturally
- this is sufficient for ordinary shell tools (which account for 90% of the time)
- `tool_timeout` provides the backstop

> If WASM cancellation becomes a hard requirement it can be addressed separately in ADR-046.

## 4. Frontend UX

### 4.1 Progressive escalation (the core design)

There are two UI states during tool execution, **switched by "has a heartbeat been received"**:

| Stage | Trigger | UI form | Applies to |
|---|---|---|---|
| **Phase A** (existing) | tool_call persisted → before the first heartbeat | a breathing grey dot + a "Running…" label | short commands finishing within 5s |
| **Phase B** (new) | after the first heartbeat arrives | grey dot + **timer** + **progress bar** + **cancel button** | long commands of 5s+ |

The switch is decided by the store's `progressByToolCallId.has(tool_call_id)` — zero network jitter
(the heartbeat fires at 5s and the frontend escalates at 5s, error < 100ms).

**Short-command experience (no regression)**:

```
Before:  ⏳ Running ls …        (grey dot)
After:   ⏳ Running ls …        (grey dot, unchanged — the heartbeat already stopped before 5s,
                               so Phase B never triggers)
```

**Long-command experience**:

```
Before:  ⏳ Running cargo build…  (grey dot, 0 feedback)
After:   ⏳ Running cargo build…  (grey dot, 0~5s)
         ⏳ Running cargo build…  (0:05 / 10:00  ▓░░░░░░░░░░  0.8%  [X])   ← triggers at 5s
         ⏳ Running cargo build…  (0:10 / 10:00  ▓░░░░░░░░░░  1.7%  [X])   ← triggers at 10s
         …
```

### 4.2 The ExploreBlock changes (`apps/acowork-desktop/src/components/chat/ExploreBlock.tsx`)

`ToolCallItem` renders different branches based on `hasProgress`:

```tsx
// Phase A: before 5s
{isPendingResult && !hasProgress && (
  <span className="... animate-pulse rounded-full bg-zinc-300" />
)}

// Phase B: after 5s
{isPendingResult && hasProgress && (
  <>
    {/* 1. The timer (based on elapsed_ms in the heartbeat event) */}
    <span className="text-zinc-500 font-mono text-[10px] tabular-nums">
      {formatElapsed(elapsed_ms)} / {formatElapsed(timeout_ms)}
    </span>

    {/* 2. The progress bar (based on elapsed_ms / timeout_ms) */}
    <div className="h-1 w-12 rounded bg-zinc-200 dark:bg-zinc-600 overflow-hidden">
      <div
        className="h-full bg-amber-400 dark:bg-amber-500 transition-all"
        style={{ width: `${Math.min(100, (elapsed_ms / timeout_ms) * 100)}%` }}
      />
    </div>

    {/* 3. The cancel button */}
    <button
      onClick={handleCancel}
      disabled={cancelling}
      className="text-zinc-400 hover:text-red-500 transition-colors"
      title="Cancel this tool"
    >
      <X className="h-3 w-3" />
    </button>
  </>
)}
```

- The **timer** only updates when a heartbeat arrives (it does not increment locally), avoiding
  desync between local time and the server; `tabular-nums` prevents digit jitter
- The **progress bar** uses amber rather than the accent/green color — amber carries the
  "warning / attention" semantics, consistent with the "the tool is stuck" context
- A **progress bar at 100%** needs no special handling — by then `tool_timeout` has usually fired
  and the UI naturally switches to "Timed out"
- The **cancel button** disables itself and prevents double-clicks; after cancel the frontend waits
  for the `tool_result` event and the grey dot collapses automatically

### 4.3 Upgrading the ExploreBlock header's "running" hint

The existing "Exploring... (N steps)" at `ExploreBlock.tsx:343-348` stays unchanged (it is a
step-level hint). But **a new auxiliary message after 5s** is added:

```tsx
{!expanded && !hasFollowUpReply && hasLongRunningTools && (
  <>{" · "}<span className="text-amber-600">{t("exploreBlock.longRunning")}</span></>
)}
```

It only shows when `progressByToolCallId.size > 0` (i.e. at least one tool has received a
heartbeat), hinting to the user that "a tool is running".

### 4.4 Store handling

`apps/acowork-desktop/src/stores/chat-store.ts` adds:

```ts
interface PendingToolProgress {
  tool_call_id: string;
  elapsed_ms: number;
  timeout_ms: number;
  received_at: number;
}

// the reducer on the ToolProgress event:
state.progressByToolCallId.set(event.tool_call_id, { ... });
```

Stored in a `Map<tool_call_id, PendingToolProgress>`, cleared when the `tool_result` arrives.

### 4.5 Sending the cancel

Reuse `mqtt_publish_control` + `command: "cancel_tool"`:

```ts
await invoke("mqtt_publish_control", {
  agentId,
  command: "cancel_tool",
  payloadJson: {
    session_id: currentSessionId,
    tool_call_id: call.tool_call_id,
  },
});
```

No Tauri Rust backend change is needed (the command name is dispatched dynamically as a string).

## 5. Protocol Compatibility

### 5.1 Backward compatibility

- `ChunkEvent::ToolProgress` is a new variant; an old frontend receiving it falls into the unmatched
  branch and **ignores it without error**
- `UserOp::CancelTool` / `ControlAction::CancelTool` are new variants; an old runtime receiving them
  fails `parse_control_payload` → log + ignore
- No existing event is affected

### 5.2 Topic and QoS

- It uses the `acowork/agents/{id}/chunks/{sid}` topic with QoS 0 (at-most-once), consistent with
  the existing control events
- Losing a heartbeat is acceptable; the next one arrives 5s later

## 6. Trade-offs

### 6.1 Why not open a new topic besides `RecordComplete`?

- The topic explosion problem (see ADR-035 D2.1)
- A heartbeat is not an independent data flow, it is a control signal → fold it into `chunks/{sid}`
  alongside the existing `RecordComplete`
- The cost of a lost heartbeat is very low (one 5s progress display)

### 6.2 Why a `watch` channel instead of an `AtomicBool`?

- `watch::Sender` can be cloned and cancelled multiple times (useful if "cancel all tools in a
  batch" is ever needed)
- `wait_for()` is cancel-safe, making future timeout combinations easier
- A token is currently ~24B of HashMap entry — negligible overhead

### 6.3 Why not change the `tool_timeout_ms` default?

- The user raised a frontend experience problem, not a configuration problem
- Changing the default would mask the root cause (the missing heartbeat)
- ADR-045 does not touch configuration; it only adds the frontend signal

### 6.4 Why not use HTTP?

- A cancel is one-shot, must-not-be-lost and needs no history — MQTT QoS 0 is sufficient
- It goes through the same path as `approval_decision` (`ChatPanel.tsx:1084`), giving the frontend a
  consistent mental model
- Using HTTP would instead introduce the inconsistency of "why do stop/approve go over MQTT while
  cancel goes over HTTP"

## 7. Implementation Steps (incremental delivery)

> Strictly following the project's "incremental delivery" engineering principle, each step is a
> reviewable small diff.

**Step 0 — the protocol layer ✅**: add `ControlAction::CancelTool { session_id, tool_call_id }` in
`control_handler.rs`; add `UserOp::CancelTool` in `inbound.rs`; add the `control_action_to_inbound`
match branch in `gateway_loop.rs`; add the constant
`acowork_core::timeout_config::constants::TOOL_HEARTBEAT = 5s`; unit test
`test_parse_control_cancel_tool` (cargo check --tests passes).

**Step 1 — the Runtime cancellation path ✅**: add the `pending_tool_cancels` field to `AgentLoop`;
add the real `cancel_tool_by_id` implementation in `loop_inbound.rs` (replacing the stub); create a
`watch::channel` outside the spawn in `loop_tools.rs` and register it into `pending_tool_cancels`;
add a `tokio::select!` wrapping `cancel_rx.wait_for` in `loop_tools.rs`; unit test: the child process
is killed after cancelling a shell process (P1, next round).

**Step 2 — heartbeat emission ✅**: add the `ToolProgressPayload` message + `session_message::Event`
oneof field 32 in `mqtt_payload.proto`; add the `publish_tool_progress` method to
`MqttChunkPublisher`; add the `ToolProgress` variant to `ChunkEvent`; add the matching branch in
`subsystems.rs`'s `relay_chunk_event_mqtt`; add the heartbeat task (5s interval, skipping the first
tick) inside the spawn closure in `loop_tools.rs`; add
`session_message::Event::ToolProgress` JSON serialization in `chat_mqtt.rs` (Tauri); add the
`control_command::Command::CancelTool` route in `mqtt_client.rs` (Tauri); unit test: observe at
least one heartbeat during a long tool (P1, next round).

**Step 3 — the frontend store ✅**: add the
`toolProgress: Record<tool_call_id, { elapsedMs; timeoutMs }>` field to `chatStore.ts`; handle the
incremental update of the `tool_progress` ChunkEvent and clear the corresponding entry once
`tool_result` arrives; unit test: the entry is cleared after `tool_result` arrives (P1, next round).

**Step 4 — the frontend UI ✅**: add the timer / progress bar / cancel button to `ToolCallItem`; wire
the cancel button to `chatStore.cancelTool` → `mqtt_publish_control cancel_tool`; only escalate to
Phase B when `toolProgress[id]` exists — tools finishing within 5s keep the original UX (the Phase A
grey dot); manual test: issue a 30s+ sleep command, observe the heartbeat display in the UI, and
confirm clicking cancel stops the process.

**Step 5 — documentation and ADR archiving ✅**: add ToolProgress / cancel_tool to
`docs/protocols/zh/mqtt.md` §3.2 + §9.3; sync `docs/protocols/zh/README.md` §3's sequence and §5's
navigation; update this ADR's status to "Implemented".

## 8. Verification

| Scenario | Expectation |
|---|---|
| the `sleep 120` shell tool | the first heartbeat is seen after 8–9s and the UI shows "1m 12s / 10m 0s" |
| clicking the cancel button (after 10s) | the tool process disappears within ≤500ms (**critical**), the `tool_result` contains "Cancelled by user after 10s", and the LLM continues reasoning |
| a WASM tool (no cancel support) | the cancel event arrives but the UI still shows it as running; `tool_timeout` is the backstop |
| a lost heartbeat (the broker is temporarily down) | the UI does not update (at most 5s without a refresh) and does not crash |
| an old frontend + a new runtime | ToolProgress is received and ignored, with no functional impact |
| an old runtime + a new frontend | the received `cancel_tool` command is logged and dropped, and the tool completes naturally |

## 9. Follow-up Topics (out of scope)

- Streaming shell stdout/stderr back to the frontend (a separate topic, requiring `shell.rs` to be
  rewritten to `tokio::process::Command`)
- WASM tool runtime cancellation (requires wasmtime's epoch interruption)
- Iteration-level pause (as distinct from tool-level cancellation)
