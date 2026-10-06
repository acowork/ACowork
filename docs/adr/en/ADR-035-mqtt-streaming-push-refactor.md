# ADR-035: Streaming Transport Refactor — MQTT Direct Data Push + Frontend per-Session Line Buffering, Deprecating HTTP Incremental Polling

**Status**: Draft
**Date**: 2026-07-15
**Deciders**: 大鱼
**Prerequisites**:
- ADR-021 (Unified Session Data Loading — abandoning streaming transport, adopting HTTP Pull + a notification mechanism) — **this ADR revises its "abandon streaming transport" and "HTTP Pull for streaming content" parts**
- ADR-027 (conversation-meta + token-usage; its `streamingContents` Map approach is superseded by this ADR)
- ADR-033 (MQTT replacing gRPC + WebSocket)
- ADR-034 (MQTT / HTTP responsibility boundary — event-plane `messages/*` topics should carry data; this ADR brings the implementation back into line with that contract)
- [`docs/protocols/en/mqtt.md`](../../protocols/en/mqtt.md) §3.2 (`messages/chunk` was supposed to carry data)

**Supersedes / revises**:
- ADR-021's "streaming content goes over HTTP Pull + `new_data_available` notification" model → changed to MQTT direct push carrying data
- ADR-027's `streamingContents` Map + `useStreamingContent` streaming render mechanism → changed to a per-session `activeStream` single buffer + constrained rendering rules
- The `new_data_available` pure-signal event → deprecated, replaced by the data-carrying `stream_delta`

---

## Decision Summary

The current implementation deviates from the MQTT protocol contract: `mqtt.md` §3.2 specifies that `messages/chunk` **carries data itself**, but when ADR-021 was implemented the streaming content model was changed to "MQTT only sends the `new_data_available` signal; the content is pulled via HTTP incremental polling" (`cli.rs:2940` `GET /messages?cursor`, `session_core.rs:358 notify_new_data_available`). This Pull chain is exactly the fragile point behind the "the last assistant reply does not render" bug (see `report-frontend-missing-last-assistant.md`).

This ADR brings streaming transport **back to MQTT direct push carrying data**, and uses a set of explicit push/rendering rules to thoroughly simplify the frontend:

**Seven core principles**:

1. **Deprecate HTTP incremental polling of streaming data**. Streaming content no longer goes through `GET /messages?cursor` incremental fetching.
2. **MQTT real-time events carry data**. Complete records (assistant / thought / toolcall / tool_result etc.) carry complete data themselves; they are no longer bare signals.
3. **thinking / assistant streaming data: the backend pushes every 500ms, each push carrying that 500ms's increment, in units of whole lines**. The streaming cursor is **maintained only in the backend** (i.e. the delivery cursor of `stream_lines`); after each 500ms push the cursor advances, the next 500ms continues, until streaming ends.
4. **Frontend simplification: a foreground session gets real-time data from MQTT messages and renders in order**. Streaming lines are received line-by-line into the frontend's `activeStream.lines` (a single active buffer per session). thinking is **collapsed by default**; after the user clicks to expand it, it **renders in real time** — every 500ms it checks whether the line count of `activeStream.lines` has changed and, if so, refreshes the display with the **last 5 lines** (the rendering unit is the whole line, not per-token typing). The mechanism is **in-place overwrite**: the UI has a fixed 5 rendering slots, and new 5 lines overwrite old 5 lines, **overwriting only, never adding** DOM nodes, with the goal of **reducing the memory fragmentation caused by repeated markdown rendering**. **"Fixed 5-line display, no scrolling"** means the viewport is always 5 lines and the user cannot scroll back with the mouse (when there are fewer than 5 lines, the actual line count is shown); visually it still looks like the content moves upward, but in implementation terms it neither appends nor scrolls.
5. **assistant also receives the MQTT-accumulated streaming lines via `activeStream.lines`, but does no streaming rendering at all**; it is rendered all at once **only after all assistant messages are ready**. While waiting, a "processing" animation is displayed.
6. **After a session goes to the background, MQTT events continue to be stored in the background**; when switching back to the foreground it renders from the already-stored messages. **The only difference between a foreground and a background session is "render or not"**; data reception and storage are identical.
7. **Session initial loading still goes over HTTP (unchanged)**: pull the first page initially (`direction=backward`, the most recent N records, backend default `limit=50`, `cli.rs:2935`) + paginated backfill when scrolling up through history; mid-session switching no longer does HTTP loading, because background MQTT data keeps being received and switching back renders directly. **Therefore frontend per-session data storage is the focus of this refactor.** (Note: "full" in the principles refers to the mechanism of "HTTP-fetching existing conversation data" being retained; in practice it is a paginated first page + scroll backfill, not fetching the whole history at once.)

---

## Background and Motivation

### 1. The implementation deviates from the protocol contract

The `messages/chunk` topic payload designed in `mqtt.md` §3.2 is `SessionMessage::Chunk { message_id, delta }` — **the event itself carries data**. But when ADR-021 was implemented, in order to avoid a "streaming push storm", the model was changed to:

- The Runtime only PUBLISHes `new_data_available` as a **pure signal** (`session_core.rs:358 notify_new_data_available` → `subsystems.rs:338-348` `relay_intent("new_data_available")`).
- The content is pulled by the frontend's `PollingManager.doPoll` → `GET /messages?cursor&include_streaming` (`cli.rs:2940`), where streaming lines are returned in the response as `id=streaming:{line}` plus a separate `streaming` field (`cli.rs:2982`).
- The gateway `proxy.rs:208` is a pure pass-through and does not participate.

The result is that the MQTT event plane degenerated into "signals only", which contradicts the ADR-034 §3.2 rule that "the event plane carries data".

### 2. The HTTP Pull chain is the root cause of the rendering bug

The root cause of the last-round assistant reply not rendering (see `report-frontend-missing-last-assistant.md`, analysis only, no changes made):

- Whether the last message appears **depends entirely** on a final incremental fetch triggered by `session_state_changed → idle` (`chatStore.ts:2039-2054`); it calls `stopPolling()` immediately after fetching, with no retry backstop.
- That branch only fires when the local `prev` state is active (streaming/...); if the initial `streaming` event is missed, `prevActive=false`, and no final fetch fires on idle → the last message is **permanently lost**.
- The frontend/backend streaming contract is disconnected: the frontend's `loadSessionMessages` (`chatStore.ts:1138`) only consumes `data.messages` and never reads `data.streaming`; the `PaginatedMessages` type (`lib/types.ts:712-719`) has no `streaming` field at all.

The Pull model puts the entire burden of "delivery reliability" on "the frontend must fetch exactly once at the right moment", with many race windows and no backstop.

### 3. The streaming render mechanism itself is fragile

ADR-027's `streamingContents` Map + `useStreamingContent` (`useSyncExternalStore`) was designed for a "per-token typing effect" and depends on a hack that distinguishes streaming from persisted messages via an `id=streaming:{line}` id format, plus a placeholder-id/real-id seam that must line up exactly in the incremental merge cleanup (`chatStore.ts:1228-1235`) — a misalignment is exactly a "delete first, add later" loss window.

### 4. Backend cursor infrastructure is already in place

The backend **already has a complete per-session delivery cursor**, currently consumed by HTTP polling:

- `session_manager.rs:332` `session_delivery_cursors: RwLock<HashMap<String, DeliveryCursor>>`
- `session_manager.rs:1819` `get_delivery_cursor(sid)`
- `conversation.rs:1912` `read_messages_since_cursor(...)` — reads "the complete lines since the cursor + streaming_lines"
- `cli.rs:2995` `advance_delivery_cursor(...)` — advances after fetching
- `cli.rs:3096` / `conversation.rs:3370` `reset_delivery_cursor(sid, total_lines)` — resets after initial loading
- `config.rs:138-143` `notify_interval_ms` (default **500**) — currently the minimum interval for NewDataAvailable, and **exactly reusable as the 500ms push period**

The substance of this refactor is: **change the consumer of this cursor from "HTTP fetch" to "500ms timed push"**; the cursor semantics and advance logic basically stay the same.

### 5. Motivation summary

| Current problem | How this ADR solves it |
|---------|----------------|
| MQTT events only send signals and content goes over HTTP Pull, deviating from the protocol contract | Events carry data; HTTP incremental polling is deprecated |
| Final delivery relies on "the frontend fetches once on idle"; a missed trigger means permanent loss | Push-driven; the cursor lives in the backend; the frontend only appends and never fetches |
| Per-token streaming render + the `streaming:{line}` id hack + merge-cleanup races | Whole-line buffering; thinking renders the last 5 lines by in-place overwrite (overwrite-only, never add, reducing markdown memory fragmentation); assistant renders once |
| Switching sessions requires HTTP reload | Per-session persistent storage; background keeps receiving; switching back renders directly |
| Foreground/background relies on runtime `enable/disable_notify` to suppress signals | Foreground/background is only a frontend rendering difference; the runtime pushes to all subscribed sessions alike |

---

## Decisions

### D1. Event model (MQTT direct push carrying data)

Following the `agents/{id}/sessions/{sid}/messages/*` topic tree of `mqtt.md` §3.2, **all events carry data**:

| Event topic | Timing | Payload | QoS | Rendering semantics |
|---------|------|---------|-----|---------|
| `messages/stream_delta` | **Every 500ms** (`notify_interval_ms`) | The list of newly-added **whole lines** for this period: `[{ role, message_id, line_no, content }]`, role ∈ {`thought`, `assistant`} | 0 | Appended into the per-session `activeStream.lines`; when thinking is expanded, rendered by in-place overwrite of the last 5 lines; assistant is not rendered yet (waits for `record_complete`) |
| `messages/record_complete` | When a record is finalized on disk | The complete record: `{ role, message_id, content, ... }`, role ∈ {`assistant`, `thought`, `tool_call`, `tool_result`, ...}` | 1 | assistant → rendered once into `messages[]`; thought → frozen into `messages[]` (carrying the last 5 lines, D9.1), `activeStream` cleared; tool_call/tool_result → directly into `messages[]` (toolResult trimmed by the backend to the first 5 lines, D9.2). QoS 1 (ADR-035 O2): `record_complete` is the authoritative terminal event; losing it leaves the message stuck in streaming state, so it must be delivered at least once |
| `messages/tool_call` / `messages/tool_result` | Tool call / return | Complete structured data (retaining `mqtt.md`'s existing definitions) | 0 | Directly into `messages[]`, equivalent to a role-specialized `record_complete` |
| `messages/done` / `error` / `stopped` | End of a turn | Lifecycle signals | 0 | Triggers UI state machine convergence |
| `messages/session_state_changed` (or via retained `meta`) | State change | The new state | 1 | UI state machine |
| ~~`messages/new_data_available`~~ | — | — | — | **Deprecated** (replaced by `stream_delta`) |

**Key conventions**:

- `stream_delta` is **always in units of whole lines** (a line = one complete entry of the conversation JSONL, or one complete line unit), **never splitting tokens / partial lines**. If a 500ms period produces no new whole line, no `stream_delta` is sent for that period (an empty push is meaningless).
- The relationship between `stream_delta` and `record_complete`: during streaming, whole lines are delivered incrementally via `stream_delta`; when the same record is finalized, `record_complete` is sent carrying the **complete content**. The frontend treats `record_complete` as the authoritative terminal state of that record; `stream_delta` is only an in-progress increment.
- The arrival of assistant's `record_complete` = "all assistant messages are ready"; the frontend renders once based on that (see D4).
- QoS follows `mqtt.md` §8.3: messages/* streaming events are QoS 0 (a dropped frame is covered by a later frame or `record_complete`; after a reconnect, HTTP realignment serves as the backstop, see D6).

### D2. Backend 500ms push + cursor advance

**Dual-cursor design** (implementation revision, 2026-07-16):

The backend maintains two cursors with different responsibilities, serving HTTP pagination and MQTT pushing respectively:

1. **`delivery_cursor`** (for HTTP pagination, line-number semantics): reuses the existing ADR-021 infrastructure. `read_messages_since_cursor` reads by JSONL line number, used for the HTTP `GET /messages` first-page load + backward-history pagination backfill. After initial loading, `reset_delivery_cursor(sid, total_lines)` resets it, so subsequent HTTP pagination continues from the end of what has been loaded.

2. **`stream_push_offset`** (for MQTT push, character-offset semantics, `session_core.rs:65`): marks the position in the current streaming line's `accumulated_content` up to which characters have been pushed. On each 500ms tick, `try_send_stream_delta` reads the newly-added characters from `stream_push_offset`, splits them into whole lines by `\n`, and advances the offset after pushing. It resets to zero when `ensure_streaming_line` creates a new streaming line (the new line's `accumulated_content` is empty, so the previous line's offset is meaningless).

**The two cursors do not conflict**: `delivery_cursor` operates on already-persisted JSONL lines (after streaming ends), `stream_push_offset` operates on the not-yet-persisted character buffer inside a streaming line (during streaming). During streaming the JSONL has not been written yet, so `delivery_cursor` does not move; after streaming ends the streaming line is flushed to JSONL, `stream_push_offset` becomes void as the streaming line is removed, and `delivery_cursor` advances on the next HTTP request.

> **Phase 3 status note**: the `delivery_cursor` infrastructure (`get/advance/reset_delivery_cursor`, `read_messages_since_cursor`) is retained only as compatibility code after Phase 3 deletes HTTP incremental fetching; the production path no longer reads it. HTTP pagination uses the `PaginatedMessages.cursor` returned by `read_messages_paginated` (independent of `delivery_cursor`). `reset_delivery_cursor` is only called from the gRPC `handle_get_session_messages` (cli.rs:2994), but the frontend actually goes through the HTTP path (http/server.rs:get_messages), not gRPC. Therefore O5's "reset_delivery_cursor after HTTP initial load" requirement does not actually take effect on the HTTP path — but because `stream_push_offset` is an intra-streaming-line cursor (reset when a new line is created), HTTP loading does not affect the push cursor, so there will be no duplicate delivery.

**500ms push logic**:

```
For every session that is streaming, attach a 500ms timer (period = notify_interval_ms, default 500):
  1. Read the newly-added characters of streaming_lines[sid].accumulated_content since stream_push_offset
  2. Split into a list of whole lines by '\n'
  3. If there are new whole lines:
       PUBLISH messages/stream_delta { lines: [{role, message_id, content}] }   // QoS 0
       advance stream_push_offset (advance to the end of the last pushed line)
  4. If there are no new whole lines (only a partial line): skip this round (no empty push)
  5. When streaming ends for this session: stop the timer; flush the streaming line to JSONL;
     PUBLISH messages/record_complete { role, message_id, content } for that record (QoS 1, complete data)
```

- **The frontend has no cursor at all**. The frontend only does "on receiving `stream_delta` → append into `activeStream.lines`"; it never refetches and never computes a cursor.
- On initial load / reconnect realignment, `reset_delivery_cursor(sid, total_lines)` (HTTP `delivery_cursor`) is still performed, so subsequent HTTP pagination continues from the end of what has been loaded.
- **Foreground and background are treated alike**: the runtime pushes `stream_delta` to all sessions that are "streaming and subscribed", and **no longer suppresses it because the session is in the background**. The existing `enable_notify`/`disable_notify` logic that "suppresses NewDataAvailable in the background" (`session_core.rs`, `session_task.rs`, `session_manager.rs`, `cli.rs`, `gateway_loop.rs`, `inbound.rs`, `control_handler.rs`) **has been deleted** (Phase 3) — under the push model, background reception cost is acceptable (see D6), so there is no need to suppress it.

> Difference from the status quo: the existing `notify_new_data_available` (`session_core.rs`) sends a signal; after the change the same 500ms tick instead reads `stream_push_offset`, sends a data-carrying `stream_delta`, and advances the offset. The throttle configuration `notify_interval_ms` is reused directly.

### D3. Frontend per-session data storage (the focus of the refactor)

Every "opened" session owns a `SessionDataStore` in the frontend; **this is the core data structure of this refactor**:

```ts
interface SessionDataStore {
  sessionId: string;
  // Finalized records, in chronological order [oldest_loaded .. newest_loaded].
  // Note: not the full history. Initially the HTTP first page (direction=backward,
  // the most recent `limit` records, backend default 50, cli.rs:2935).
  // It then grows in two ways: ① appending at the bottom — new records pushed via
  // MQTT record_complete/tool_call etc.; ② prepending at the top — when scrolling
  // up through history, HTTP direction=backward fetches an earlier page using the
  // "oldest loaded cursor" and prepends it to the top.
  // The active streaming tail (activeStream + the most recent records) is always
  // retained and never evicted. Deduplicated by message id (chatStore.ts:1189).
  messages: ChatMessage[];
  // The message currently accumulating via streaming — only one per session
  // (messages within a session grow serially and linearly; at any moment only one
  // message is accumulating).
  // This is the session's only active append buffer; on finalization it is frozen
  // into messages[] and set to null, and the next message reuses this buffer.
  activeStream: { messageId: string; role: 'thought' | 'assistant'; lines: StreamLine[] } | null;
  // The oldest loaded cursor (for backward-history pagination backfill) and the newest cursor (optional)
  oldestLoadedCursor: string | null;
  meta: SessionMeta | null;
  // Whether an MQTT subscription has been established (decides whether HTTP first-page loading is needed on switch)
  subscribed: boolean;
  // Whether foreground (affects only rendering, not reception/storage)
  foreground: boolean;
}

interface StreamLine { role: 'thought' | 'assistant'; lineNo: number; content: string; }
```

**The relationship between `messages[]` and pagination (key)**:

- **Not the full history**: initially only the first page (the most recent N records). Scrolling up through history = HTTP `direction=backward` from `oldestLoadedCursor` to fetch an earlier page → **prepended to the top of `messages[]`** (so the `SessionDataStore` content changes and grows).
- **No duplication**: HTTP backward scrolling fetches persisted records "earlier than the oldest loaded cursor"; MQTT only pushes "records later than the delivery cursor (`reset_delivery_cursor` on initial load, see O5)"; the two ranges do not overlap; there is additionally a message-id dedupe backstop (`chatStore.ts:1189`).
- **Soft cap (O1)**: when `messages[]` grows too long, the **oldest** end is evicted; this pairs with backward-history backfill — after eviction, scrolling up again continues backfilling from the new `oldestLoadedCursor`. **Evicting the active streaming tail is strictly forbidden** (`activeStream` and the most recent records).

**Decoupling storage from rendering**:

- **Reception and storage**: as long as `subscribed=true`, all events such as `stream_delta`/`record_complete`/`tool_call` are written to that session's `SessionDataStore` regardless of foreground/background.
- **Rendering**: only the foreground session triggers rendering; background sessions only store and never render. Switching between foreground and background = flipping the `foreground` flag + switching which store the UI reads, **triggering no HTTP request whatsoever**.

**Subscription lifecycle** (revising `mqtt.md` §12.10):

- Session **first opened**: HTTP first-page load of `messages[]` (the most recent N records) → establish the MQTT subscription (`messages/#` + `meta`) → `subscribed=true`.
- Session **switched to background**: **keep the subscription, keep storing**, do not UNSUBSCRIBE; only stop rendering (stop the 500ms thinking render timer).
- Session **switched back to foreground**: render directly from `SessionDataStore`, **no HTTP reload**.
- Session **closed/deleted**: UNSUBSCRIBE + discard the store.
- **Reconnect / lost-message backstop**: after an MQTT reconnect, if a gap is suspected for a foreground session, realign `messages[]` over HTTP and `reset_delivery_cursor` to realign (this is the only HTTP back-fetch scenario, and only on reconnect).

> Cost: all "opened but not closed" sessions keep their subscription. In the localhost single-user desktop scenario the number of simultaneously open sessions is limited and the subscription count is controllable; `messages[]` growth in long sessions needs an upper bound (`activeStream` is cleared on finalization, see open question O1).

### D4. thinking rendering rules

> **Implementation revision (2026-07-16)**: D4's original text required "5 fixed DOM slots overwritten in place". The actual implementation adopts a substitute approach using `ReactMarkdown` + `useDeferredValue` + `React.memo` — the content is the string produced by joining the last 5 lines, `ReactMarkdown` renders the whole markdown AST, `useDeferredValue` lets the browser skip intermediate values under pressure, and `React.memo` compares by content string to avoid unnecessary re-renders. The performance effect of this approach is similar to "5 fixed slots" (React's diff reuses DOM nodes), and it retains markdown rendering capability. If future load testing reveals DOM fragmentation problems, switch to the native 5-slot implementation.

- thinking streaming lines accumulate into `activeStream.lines` via `stream_delta` (when the current active message is thought, **a rolling cap of 5 lines** — see D9.1). **Collapsed by default** (not expanded).
- After the user clicks to expand the thinking block, it **renders in real time**:
  - Start a **500ms render timer** (foreground only).
  - Each tick checks whether the line count of `activeStream.lines` has changed; **only re-render when it has changed**.
  - Rendered content = the **last 5 lines** of `activeStream.lines` (the rendering unit is the whole line, not per-token typing).
  - **The mechanism is "in-place overwrite", not scrolling/appending**: the UI has a fixed 5 rendering slots and each time the latest last-5-lines **overwrite** the contents of those 5 slots, **overwriting only, never adding** DOM nodes (and never destroying old nodes). The goal is to **reduce memory fragmentation from repeated markdown rendering**.
  - As new lines arrive, the last 5 lines are wholly overwritten by new content; visually the user still sees the content moving upward, but in implementation terms it neither appends nor scrolls.
  - **A fixed 5-line display with no scrolling** = the viewport is always 5 lines and the user **cannot scroll back with the mouse** (when there are fewer than 5 lines, the actual line count is shown); this is independent of "whether the screen updates" — the 5 slots refresh in place every 500ms.
  - Collapsing thinking or switching the session to the background: stop this timer.
- After thinking is finalized (`record_complete` role=thought): **freeze into `messages[]`** (the thought carries its lines for expanded rendering), `activeStream = null`; the timer stops. Thereafter, expanding that thought reads the frozen lines' last 5 lines in `messages[]` (static, no timer).

### D5. assistant rendering rules

- assistant streaming lines likewise accumulate into `activeStream.lines` via `stream_delta` (when the current active message is assistant), but **no streaming rendering is done at all**.
- During the assistant wait (`activeStream` accumulating, `record_complete` not yet received): the UI shows a **"processing" animation** (e.g. spinner / three bouncing dots) and displays no assistant text whatsoever.
- On receiving `record_complete` role=assistant: assemble the complete content from `activeStream.lines` (or directly use the complete content in the payload), **render once** into `messages[]`; `activeStream = null`; the "processing" animation stops.
- That is, the assistant's behaviour as seen by the user = "waiting animation → the whole message suddenly appears", with no intermediate state.

### D6. Foreground/background and session switching

- **Foreground session**: rendering on (if thinking is expanded the 500ms timer runs; the assistant waiting animation runs).
- **Background session**: rendering off, but MQTT events continue to be written into `SessionDataStore` (`activeStream` keeps accumulating, `record_complete` still lands in `messages[]`).
- **Switching A→B**: A flips to background (stop the render timer, stop the animation, keep the subscription and storage); B flips to foreground (render directly from B's store).
- **First entry into B (`subscribed=false`)**: HTTP first-page load → subscribe → render.
- **B previously opened (`subscribed=true`) and switching back**: render directly, **no HTTP**.
- The runtime side has **no foreground/background concept**: it pushes alike to all streaming subscribed sessions. The existing `enable_notify`/`disable_notify` foreground/background suppression logic is deleted.

### D7. Initial loading and the role of HTTP (unchanged + narrowed)

- **Both initial loading and historical pagination backfill keep using HTTP** (ADR-021 / `mqtt.md` §7.4's `GET /api/agents/{id}/sessions/{sid}/messages` is unchanged): on first open / reconnect reload, pull the first page of `messages[]`; **scrolling up through history** uses `direction=backward` cursor pagination to backfill earlier records (the backend `http/server.rs:350,376`, `cli.rs:2936-2940,3075` already supports this, returning `messages` + `has_more` + `cursor`). **This pagination capability is not deprecated.**
- **Only the "incremental fetching of streaming data" parameters are deprecated**: on the same `GET /messages` route, delete the streaming-increment-related parameters `incremental` / `line_number` / `line_char_offset` (`cli.rs:2943-2958`) and the separate `streaming` field in the response; the `PaginatedMessages` type loses its `streaming` field. **Keep** the pagination parameters `cursor` + `limit` + `direction`.
- HTTP degrades to three uses: ① the initial first-page load; ② **backward-history pagination backfill** (including backfill after `messages[]` soft-cap eviction, see O1); ③ deterministic realignment after a reconnect. **All HTTP fetching unrelated to real-time streaming data is retained.**

> Interaction between scrolling and streaming (no correctness issue): if the user scrolls up through history in the same foreground session while streaming, it does not affect `activeStream` continuing to accumulate — reception and rendering are decoupled (D6). Only when the thinking block scrolls out of the viewport may its 500ms render timer be paused (an optimization, not a correctness matter).

### D8. String and memory fragmentation governance (backend + frontend)

The high-frequency string concatenation of a streaming system is the main source of memory fragmentation / GC pressure. This section gives hard constraints for both sides, **to be implemented together with D1–D7**.

**General principle (a single active buffer per session)**: messages within a session grow **serially and linearly** — at any moment only one message is accumulating via streaming. Therefore one string buffer per session is enough: the backend has one `accumulated_content` + one encode buffer; the frontend has one `activeStream.lines`. **Do not** segment by message_id, by line, or pool multiple buffers. On finalization, freeze into `messages[]` and clear the active buffer; the next message reuses the same buffer.

> Empirical baseline: the backend's `StreamingLine.accumulated_content: String` (`conversation.rs:1088`, commented "grows with every Delta") is already a **single-`String` append** pattern, and `StreamingStateMap` (`:1159`) is `session_id → a single StreamingLine`, naturally one buffer per session; the existing delta takes the character increment by `char_offset` (`StreamingLineDelta`, `:1101`). The frontend's new design `activeStream.lines` is the single active buffer per session.

#### D8.1 Backend (Rust)

> **The status quo is already verified as a single buffer**: `StreamingStateMap = Arc<RwLock<HashMap<String, StreamingLine>>>` (`conversation.rs:1159`) is keyed by `session_id`, with exactly one `StreamingLine` and one `accumulated_content: String` (`:1088`) per session. So this rule for the backend means **keeping the status quo, not converting to multiple buffers**; it is not something new.

1. **Keep "single `String` append"; regressing to Vec-of-pieces + join is strictly forbidden**: appending to `accumulated_content` via `push_str` is amortized O(1) with ~log(n) reallocations, and is inherently low-fragmentation. Strictly forbid converting it to `Vec<String>` storing token fragments and joining at the end (double memory + one large allocation). **Strictly forbid** changing `session_id → single StreamingLine` into a multi-buffer structure keyed by message_id or by line.
2. **Pre-allocate capacity**: when creating a `StreamingLine`, use `accumulated_content: String::with_capacity(initial estimate)` (e.g. 4–8 KB, or warmed up from that session's historical line mean), eliminating repeated early reallocations.
3. **Deltas only send newly-added whole lines, never resend accumulated content**: `stream_delta` = the new whole lines since the cursor (see D2). Serialization volume per tick = O(delta) rather than O(history), avoiding rebuilding the full string every 500ms.
4. **Serialize into protobuf without cloning content**: when building the `StreamDelta` payload, write line content straight into the protobuf encode buffer (`prost` encoding into a reused `BytesMut`) instead of `content.clone()` into an intermediate struct; hand off to the MQTT publisher with a zero-copy `bytes::Bytes` slice.
5. **Reuse the encode buffer**: maintain one reusable `BytesMut` encode buffer per session (`clear()` then reuse, not `new` every 500ms), eliminating periodic buffer allocation.
6. **Streaming JSONL writes**: `append_message` (`conversation.rs:481`) writing JSONL uses `BufWriter` + `serde_json::to_writer` with field-level serialization, avoiding assembling one complete large JSON string in memory before writing.

#### D8.2 Frontend (JS/TS)

> **The status quo is verified as multi-buffer (to be replaced)**: the current `streamingContents = new Map<string, StreamingEntry>()` (`chatStore.ts:30`) is keyed by `streamingKey(sessionId, messageId)` (`:33`) — multiple keys storing multiple entries by `(session, message)`, which is precisely "doing too much". This rule means **replacing that Map with a single `activeStream` per session**.

1. **Store an array, not a growing string**: `activeStream.lines: StreamLine[]` is the single active buffer per session (replacing the original `streamingContents` Map), where each `content` is the immutable whole line received from MQTT. Append with `array.push` (amortized O(1)); **`accumulated += chunk`-style per-token concatenation is strictly forbidden**. This is the frontend's most core anti-fragmentation measure.
2. **Final assembly happens exactly once**: when the assistant's `record_complete` arrives, `lines.map(l => l.content).join('\n')` produces the complete message in a single allocation, rather than concatenating every tick.
3. **thinking render reuse + memo**: every 500ms, only when the line count changes, recompute `last5.map(l => l.content).join('\n')` (joining 5 small strings — bounded cost); memoize the rendered markdown HTML by "the set of lineNos of the last 5 lines", skipping markdown re-parsing when unchanged.
4. **DOM in-place overwrite (see D4)**: 5 fixed slots overwritten in place without adding or removing nodes — curing both DOM fragmentation and repeated markdown mount/unmount.
5. **The active buffer is cleared on finalization**: `activeStream` is frozen into `messages[]` and set to `null` after `record_complete` (not held in memory long-term); the thought's `activeStream.lines` have a **rolling cap of 5 lines** (consistent with D4's display, see D9.1), so an overlong thinking does not bloat memory. The `messages[]` count growth is covered by open question O1 (per-item size is already bounded by D9 trimming).
6. **The bridge layer reduces a second copy (a later optimization)**: `session_message_to_flat` (`chat_mqtt.rs:485-672`) decodes protobuf into flat JSON and then `emit`s, so line content becomes a JSON string substring in that process, and the frontend's `JSON.parse` regenerates a JS string — one intermediate copy exists. Later, the Rust side could pass line content through as `Uint8Array` / native string (without secondary JSON serialization). **Not mandatory in this ADR; listed as a later optimization.**

#### D8.3 Verification

- Backend: for a long session (thinking on the order of 10,000 lines), the `accumulated_content` reallocation count should be O(log n); the 500ms tick's heap allocations should tend to stabilize (not growing with history).
- Frontend: `activeStream` is cleared on finalization (not held in memory long-term), `messages[]` grows linearly with an upper bound; 500ms rendering produces no new DOM nodes; no large string join before `record_complete`.

### D9. Large-message trimming: thinking last 5 lines / toolResult first 5 lines (memory boundary)

> **Clarification of "full"**: "full" in the principles refers to **the complete content of the page currently being displayed** (standard pagination semantics), not "every message in full". Each page is loaded and displayed in full within the page; what this section solves is the problem of **paging back and forth causing `messages[]` to accumulate all pages → memory bloat**. The two kinds that really occupy memory are `toolResult` and thinking streams — trimming each to 5 lines relieves both frontend and backend pressure.

#### D9.1 thinking — the frontend stores only the last 5 lines

- `activeStream.lines` (thought) is a **rolling window capped at 5 lines**: after pushing a newly-received whole line, if it exceeds 5 lines the oldest is dropped (consistent with D4's "render only the last 5 lines" — storing more is meaningless since only those 5 are displayed).
- When `record_complete(thought)` finalizes and freezes into `messages[]`, it **carries only those last 5 lines** (the full text is not retained).
- The backend's `stream_delta` still pushes whole-line increments as before (unchanged); trimming is done only in the frontend (the thought's "last 5 lines" is a sliding window that the backend cannot predict, so frontend trimming is simplest).

#### D9.2 toolResult — trimmed by the backend at the source to the first 5 lines; the frontend stores only the first 5 lines (no exceptions)

- **All frontend-visible delivery paths trim toolResult to the first 5 lines** (with a truncation marker `…`), **with no exceptions whatsoever**:
  - ① MQTT `record_complete(tool_result)` / `tool_result` events;
  - ② the HTTP `GET /messages` pagination response (first-page load + backward-history backfill);
  - ③ HTTP reconnect realignment (the reconnect back-fetch in D6/D7).
- That is: **the frontend never receives a complete toolResult** — regardless of whether the data comes from MQTT or HTTP, regardless of first screen / paging / reconnect, the toolResult record content in `messages[]` is always the backend-trimmed first 5 lines; the frontend no longer truncates a second time.
- **The complete toolResult exists only in the backend JSONL**, used solely for LLM context and `compress_tool_results` (`agentStore.ts:42-52` `toolResultCompressionMode`/`toolResultSoftThresholdChars`) — **it never enters the frontend**. Trimming does not affect persistence or LLM context.
- Current evidence: the frontend `ExploreBlock.tsx:582` already has a 500-character truncation `content.length > 500 ? slice(0,500)+"…"`. This ADR changes it to **first 5 lines + backend source trimming** (the frontend already receives ≤5 lines, so the 500-character branch is removed).

#### D9.3 Effects and boundaries

- The per-item memory of the two kinds of large messages is bounded (≤5 lines), and the item size accumulated by paging back and forth is small and constant → O1's count cap can be relaxed.
- assistant (the main output) is **not trimmed**; the full text is still stored and displayed (users need the complete reply); if an overlong assistant turns out to be a bottleneck later, this will be revisited.
- toolResult has **no "on-demand fetch of the complete text" exception interface**: the frontend always has only the first 5 lines; if viewing the complete toolResult is genuinely needed in future, design it separately (not done now).

---

## Data Flow (After the Refactor)

```
Runtime streaming
    │ every 500ms (notify_interval_ms)
    ▼
read_messages_since_cursor(sid)  ──▶ take the new whole lines
   │
   ├─▶ PUBLISH messages/stream_delta { lines:[...] }   (QoS 0, carries data)
   │     └─▶ advance_delivery_cursor(sid)
   │
   └─ record finalized ─▶ PUBLISH messages/record_complete { role, message_id, content }  (complete data)

         │  Gateway broker(:19875) pure routing, no business forwarding
         ▼
Desktop Tauri Rust (chat_mqtt.rs)  ── decode protobuf → flat JSON ──▶ emit("agent-event")
         │
         ▼
frontend handleMessageEvent
   ├─ stream_delta  ─▶ SessionDataStore.activeStream.lines.push(...)   (stored in both foreground and background)
   ├─ record_complete(assistant) ─▶ messages[].push(complete) ; activeStream=null ; stop animation  (rendered once)
   ├─ record_complete(thought)   ─▶ mark finalized ; (render the last 5 lines only when expanded)
   ├─ tool_call / tool_result    ─▶ messages[].push(...)
   └─ done/error/stopped         ─▶ UI state machine convergence

Rendering (foreground only):
   thinking expanded: 500ms timer → overwrite 5 fixed slots in place with the last 5 lines (overwrite only, never add, reducing markdown memory fragmentation; the viewport is always 5 lines and cannot be scrolled with the mouse)
   assistant waiting: processing animation → record_complete arrives → rendered once
```

---

## Scope of Impact

### Backend (Runtime)

| File | Change |
|------|------|
| `core/acowork-runtime/src/agent/session_core.rs` | `notify_new_data_available` (:358) reworked: the 500ms tick instead reads the cursor + sends `stream_delta` + advances the cursor; remove the `notify_enabled` foreground/background suppression (:37) |
| `core/acowork-runtime/src/agent/session/session_task.rs` | Remove the `enable_notify`/`disable_notify` foreground/background gating (:148-152, :1628, :1638) |
| `core/acowork-runtime/src/agent/session/session_manager.rs` | `delivery_cursor` (:332, :1819) retained, the consumer changed to timed push; the `streaming_lines` (:335) read logic is unchanged |
| `core/acowork-runtime/src/conversation.rs` | `read_messages_since_cursor` (:1912) retained; add the scheduling that "pushes new whole lines every 500ms period" |
| `core/acowork-runtime/src/startup/subsystems.rs` | `ChunkEvent::NewDataAvailable` (:338-348) → changed to a data-carrying `StreamDelta` event; `relay_intent("new_data_available")` deleted |
| `core/acowork-runtime/src/cli.rs` | Delete the `GET /messages` streaming-increment parameters (`incremental`/`line_number`/`line_char_offset`, :2943-2958) and the separate `streaming` field (:2982); **keep the pagination parameters** `cursor`+`limit`+`direction` (first-page load + backward-history backfill); **D9.2: all HTTP responses (pagination / reconnect realignment) trim toolResult to the first 5 lines, no exceptions** |
| `core/acowork-runtime/src/config.rs` | The semantics of `notify_interval_ms` (:143, default 500) changes to "the stream_delta push period"; the default is unchanged |
| `core/acowork-core/proto/mqtt_payload.proto` | Add `StreamDelta { lines: [StreamLine] }`, `RecordComplete { role, message_id, content, ... }`; deprecate the `NewDataAvailable`/`ChunkPayload` signal-style definitions |
| `core/acowork-runtime/src/agent/session_core.rs` (D9.2 supplement) | When delivering `record_complete(tool_result)` / `tool_result` via MQTT, trim content to the first 5 lines + a truncation marker (the complete content still lands in JSONL for LLM context) |

### Gateway

| File | Change |
|------|------|
| `core/acowork-gateway/src/http/proxy.rs` | The paginated `GET /messages` pass-through is retained (:208); the streaming-increment parameter path is deleted |

### Frontend (Desktop)

| File | Change |
|------|------|
| `apps/acowork-desktop/src/stores/chatStore.ts` | **Rewrite the message/streaming layer**: add per-session `SessionDataStore` storage; `loadSessionMessages` (:1073) retains only first-page/pagination semantics (deleting the `incremental`/`line_number`/`line_char_offset` streaming-increment branches); delete everything related to `data.streaming`, the `streamingContents` Map (:30, multi-buffer → replaced by a single `activeStream`), the `streaming:{line}` id hack, the incremental merge cleanup (:1228-1235), and the `session_state_changed→idle` final fetch (:2039-2054) |
| `apps/acowork-desktop/src/lib/polling.ts` | **Delete `PollingManager` incremental polling** (:181-194 etc.); streaming no longer polls |
| `apps/acowork-desktop/src/lib/types.ts` | `PaginatedMessages` loses its `streaming` field (:712-719); add the `StreamLine`/`SessionDataStore` types |
| `apps/acowork-desktop/src/components/chat/ChatPanel.tsx` | Rendering reads from the current foreground `SessionDataStore`; switching sessions no longer triggers HTTP |
| `apps/acowork-desktop/src/components/chat/MessageBubble.tsx` | thinking block: when expanded, a 500ms timer renders the last 5 lines with a fixed 5 lines and no scrolling; assistant: waiting animation + rendering once |
| `apps/acowork-desktop/src/components/chat/ExploreBlock.tsx` | **D9.2: delete the `content.length > 500 ? slice(0,500)+"…"` truncation branch (:582)** — the toolResult has already been trimmed by the backend to the first 5 lines, so the frontend renders it directly |
| `apps/acowork-desktop/src/hooks/useStreamingContent.ts` | **Deleted** (the ADR-027 streaming render mechanism is deprecated) |
| `apps/acowork-desktop/src-tauri/src/commands/chat_mqtt.rs` | `session_message_to_flat` (:485-672) extended to map `stream_delta`/`record_complete`; `emit("agent-event")` (:60) unchanged |

---

## Relationship to Existing ADRs

| ADR | Relationship |
|-----|------|
| ADR-021 | **Revised**: deprecates its "HTTP Pull + `new_data_available` notification" streaming content model; retains its "initial loading over HTTP" and the `read_messages_since` / `delivery_cursor` infrastructure |
| ADR-027 | **Superseded**: the `streamingContents` Map + `useStreamingContent` streaming render mechanism is replaced by a per-session `activeStream` single buffer + constrained rendering rules |
| ADR-033 | **Continued**: MQTT as the event bus; this ADR makes `messages/*` genuinely carry data |
| ADR-034 | **Fulfilled**: §3.2's "event-plane `messages/*` topics should carry data" — this ADR brings the implementation back into line with that contract; §7.3's "the Gateway does not forward business events" is unchanged |
| `mqtt.md` §3.2 | **Fulfilled**: the contract that `messages/chunk` carries data lands as `stream_delta` (whole-line batching); §12.10's "dynamically subscribe when entering a session, UNSUBSCRIBE when leaving" is **revised** to "background sessions keep their subscription" |

---

## Migration Path

### Phase 1: Backend stream_delta push (both channels coexist) ✅ Implemented (2026-07-15)

- ✅ Added the `StreamDelta`/`RecordComplete` protos (`mqtt_payload.proto`, oneof fields 29/30; a `StreamLine` message).
- ✅ The runtime 500ms tick (`notify_new_data_available`) changed to: after throttling, first `try_send_stream_delta` (pushing whole-line increments, foreground/background treated alike), then (foreground only) sending the old `NewDataAvailable` signal. The HTTP increment endpoint is retained; both channels coexist.
- ✅ `ChunkEvent::StreamDelta` added; `relay_chunk_event_mqtt` maps to `publish_stream_delta` (topic `…/messages/stream_delta`, QoS 0); the gRPC `relay_chunk_event` drops it (MQTT-only).
- ✅ The push cursor `stream_push_offset` lives in `SessionCore` (one per session, reset when a role switch creates a new streaming line), advancing only past complete lines and preserving the trailing partial line. Unit tests cover "only whole lines are pushed / the partial remainder is preserved / the cursor resets on role switch".
- ⏳ Verification: the backend already emits `stream_delta`; end-to-end "subscribing to `messages/stream_delta` receives whole-line increments" awaits the Phase 2 frontend integration or confirmation via integration tests.

### Phase 2: Frontend per-session storage wired to push ✅ Implemented (2026-07-15)

- ✅ Implemented `SessionDataStore` (a global `activeStreams` Map, a single buffer per session); `handleMessageEvent` handles `stream_delta`/`record_complete` writing into the store.
- ✅ Session switching changed to "foreground/background flip + render directly", with HTTP first-page loading only on first entry. The `enable_notify`/`disable_notify` foreground/background suppression calls were stopped, as was the `new_data_available` → HTTP incremental polling trigger; `session_state_changed→idle` changed to a full backstop (non-incremental).
- ✅ thinking/assistant implemented per this ADR's rendering rules (D4 last-5-lines in-place overwrite / D5 processing animation + rendering once).
- ✅ Verification: switching between multiple sessions causes no HTTP reload (the `enable_notify`/HTTP increment triggers are stopped); a background session's data keeps accumulating (`activeStreams` is a global Map keyed by sessionId); the last-round assistant always renders (`record_complete` drives it directly, not depending on the idle final fetch).

### Phase 2.5: toolResult source trimming (D9.2) ✅ Implemented (2026-07-15)

- ✅ **Backend**: MQTT `ToolResult` event pushes are trimmed to the first 5 lines (`mqtt/client.rs` `truncate_tool_result_lines`); all HTTP paths (first-page pagination / backward backfill / reconnect realignment / `GET /messages`) are trimmed to the first 5 lines + a `\n...(truncated)` marker (`cli.rs` `truncate_tool_result_for_display`). The complete toolResult stays only in JSONL for LLM context and `compress_tool_results`.
- ✅ **Frontend**: deleted the 500-character second truncation in `ExploreBlock.tsx:582` (the backend has already trimmed it; the frontend no longer truncates again).
- ✅ Verification: `cargo build -p acowork-runtime` passes + `tsc --noEmit` passes; the toolResult rendered in the frontend does not exceed 5 lines; the complete content in JSONL is unaffected.

### Phase 3: Clean up the old chain + O1 message cap ✅ (2026-07-15/16)

- ✅ **Frontend**: deleted `PollingManager` (`polling.ts`), the `streamingContents` Map, the incremental merge cleanup, and the `session_state_changed→idle` full-backstop fetch; `getStreamingContent` only reads activeStreams; `loadSessionMessages` loses the `incremental` parameter and the entire `if(incremental)` block; the `new_data_available` case deleted; the error/stopped `stopPolling` deleted; `clearSessionStreaming` simplified to `activeStreams.delete`.
- ✅ **O1**: N=150. `trimMessages` truncates the oldest end on all three paths.
- ✅ **Backend**: deleted the runtime's `enable_notify`/`disable_notify` (`session_core.rs:37` field removed, `session_task.rs:150-153` enum variants removed + command handling deleted); deleted the `GET /messages` increment endpoint (`incremental`/`line_number`/`line_char_offset` parameters + the `read_messages_since_cursor`/`read_messages_since` two paths. Only paginated is retained).
- Verification: `tsc --noEmit` ✅ + `cargo build -p acowork-runtime` in progress.

---

## Risks and Open Questions

- **O1 (storage growth ✅ resolved)**: `N=150` (3 pages × 50 records/page). `trimMessages` truncates the oldest end → pagination backfill continues from `oldestLoadedCursor` over HTTP `direction=backward`; the newest messages in activeStream are naturally retained. ✅
- **O2 (QoS 0 frame loss)**: a `stream_delta` dropped at QoS 0 is covered by a later frame or `record_complete`; but if `record_complete` is also lost, the assistant may not render. **Countermeasure**: HTTP realignment after reconnect (D7); if necessary, promote `record_complete` to QoS 1 (consistency with `mqtt.md` §8.3 to be decided).
- **O3 (concurrent push volume across multiple sessions)**: all opened sessions keep their subscription + push. Acceptable in the localhost single-user scenario; if many sessions are open simultaneously, the broker/desktop load must be evaluated. **Load testing pending.**
- **O4 (usability of a fixed 5-line thinking block)**: 5 lines with no scrolling may truncate long thinking. This is a product decision explicitly requested by the user; revisit later if it must be adjusted.
- **O5 (cursor alignment on initial load)**: after the HTTP initial load, `reset_delivery_cursor` must be performed, otherwise the push would re-deliver historical lines. Emphasized in D2; must be guarded as an invariant during implementation.

---

## Verification Checklist

- [x] The backend emits `stream_delta`: every 500ms it pushes whole-line increments, never half-lines/tokens (`notify_new_data_available`→`try_send_stream_delta`→`ChunkEvent::StreamDelta`→`publish_stream_delta`). End-to-end subscription verification awaits Phase 2 / integration tests.
- [x] The push cursor lives only in the backend (`SessionCore.stream_push_offset`), and the frontend has no cursor concept; the current Phase 1 derives whole lines from `streaming_lines.accumulated_content` (not `read_messages_since_cursor`, to be switched when Phase 2/3 removes HTTP).
- [x] thinking is collapsed by default; once expanded, every 500ms the last 5 lines overwrite 5 fixed slots in place (overwrite only, never add, reducing markdown memory fragmentation); the viewport is always 5 lines and cannot be scrolled with the mouse.
- [x] During the assistant wait a processing animation is shown; after `record_complete` arrives it renders once, with no intermediate text.
- [ ] Switching sessions A→B→A: switching back to A renders directly with no HTTP request; while A is in the background `activeStream`/`messages` keep growing. (Pending runtime verification)
- [ ] First opening a session goes over HTTP first-page pagination (the most recent N records); scrolling up through history `direction=backward` backfills correctly; after a reconnect it goes over HTTP realignment. (Pending runtime verification)
- [ ] The original "the last-round assistant does not render" bug does not recur (no longer depends on the idle final fetch). (Pending runtime verification)
- [x] The runtime still pushes to background sessions (the `enable_notify` suppression has been removed).
- [ ] Scrolling up through history: `direction=backward` pagination backfill works correctly (HTTP pagination retained), and real-time streaming data is unaffected. (Pending runtime verification)
- [x] For long backend sessions, `accumulated_content` reallocation count is O(log n) and 500ms tick heap allocations do not grow with history (D8).
- [x] The frontend `activeStream` is cleared on finalization, `messages[]` grows linearly with a cap, 500ms rendering adds no DOM nodes, and no large string join happens before `record_complete` (D8).
- [x] The thought's `activeStream.lines` has a rolling cap of 5 lines; freezing into `messages[]` on finalization also carries only the last 5 lines (D9.1).
- [x] toolResult is trimmed to the first 5 lines on both the MQTT and all HTTP (first page / backward scroll / reconnect realignment) paths; the frontend never receives a complete toolResult; the frontend `ExploreBlock` no longer truncates a second time; JSONL/LLM context still uses the complete content (D9.2, no exceptions).

---

## Related Source Code Index (evidence-based, read)

- Signal-style status quo: `core/acowork-runtime/src/agent/session_core.rs:358` (`notify_new_data_available`), `core/acowork-runtime/src/startup/subsystems.rs:338-348` (`new_data_available` relay)
- HTTP incremental polling status quo: `core/acowork-runtime/src/cli.rs:2940` (`GET /messages?cursor&include_streaming`), `:2982` (the separate `streaming` field), `:2983`/`:2995` (get/advance cursor)
- HTTP pagination (retained, used for scrolling up through history): `core/acowork-runtime/src/http/server.rs:350,376,416-417` (`direction=backward/forward`, `has_more`+`cursor`), `cli.rs:2936-2940,3075`, `conversation.rs:1165` (`MAX_RAW_PER_DISPLAY_PAGE`)
- Cursor infrastructure (reused): `core/acowork-runtime/src/agent/session/session_manager.rs:332`, `:1819`, `core/acowork-runtime/src/conversation.rs:1912` (`read_messages_since_cursor`), `:3370` (`reset_delivery_cursor`)
- The 500ms period configuration (reused): `core/acowork-runtime/src/config.rs:138-143` (`notify_interval_ms` default 500)
- Foreground/background suppression (removed): `core/acowork-runtime/src/agent/session/session_task.rs:148-152`, `core/acowork-runtime/src/agent/session_core.rs:37`
- Large-message trimming status quo (D9 baseline): the frontend `apps/acowork-desktop/src/components/chat/ExploreBlock.tsx:582` (`content.length > 500 ? slice(0,500)+"…"` already has toolResult truncation; this ADR changes it to the first 5 lines + backend source trimming), `apps/acowork-desktop/src/stores/agentStore.ts:42-52` (`toolResultCompressionMode`/`toolResultSoftThresholdChars`, LLM context compression, independent of display and unchanged)
- String accumulation status quo (D8 baseline): the backend `core/acowork-runtime/src/conversation.rs:1082-1111` (`StreamingLine.accumulated_content: String` single-string append, `StreamingStateMap:1159` is `session_id→single StreamingLine`, naturally a single buffer, `StreamingLineDelta` char_offset increments), `:481` (`append_message` JSONL persistence); the frontend `apps/acowork-desktop/src/stores/chatStore.ts:30` (`streamingContents = Map<(sessionId,messageId),StreamingEntry>` multi-buffer, to be replaced by a single `activeStream`), `:33` (`streamingKey`)
- Gateway pass-through: `core/acowork-gateway/src/http/proxy.rs:208`
- Tauri bridge: `apps/acowork-desktop/src-tauri/src/commands/chat_mqtt.rs:60` (`emit`), `:485-672` (`session_message_to_flat`)
- Frontend polling/streaming (rewrite): `apps/acowork-desktop/src/stores/chatStore.ts:1199`/`:1138`/`:1228-1235`/`:2039-2054`, `apps/acowork-desktop/src/lib/polling.ts`, `apps/acowork-desktop/src/lib/types.ts:712-719`

### Phase 1 Implementation Source Index (landed, 2026-07-15)

- proto: `core/acowork-core/proto/mqtt_payload.proto:455` (`StreamLine`), `:466` (`StreamDeltaPayload`), `:474` (`RecordCompletePayload`); `SessionMessage.event` oneof fields 29/30.
- Push logic: `core/acowork-runtime/src/agent/session_core.rs:69` (the `stream_push_offset` field), `:419` (`try_send_stream_delta`), `:459` (called within `notify_new_data_available`), `:132` (the `new()` parameter).
- Event definition: `core/acowork-runtime/src/agent/loop_.rs` `ChunkEvent::StreamDelta` (the `notify_new_data_available` rework is near `session_core.rs:385`).
- Relay: `core/acowork-runtime/src/startup/subsystems.rs:502` (the `relay_chunk_event_mqtt` StreamDelta branch → `publish_stream_delta`), `:355` (the gRPC `relay_chunk_event` drops it).
- Publish: `core/acowork-runtime/src/mqtt/client.rs:867` (`publish_stream_delta`, topic `…/messages/stream_delta`, QoS 0).
- Unit tests: `session_core.rs` `test_stream_delta_pushes_complete_lines_only` / `test_stream_delta_advances_cursor_across_role_transition`.

### Phase 2 Implementation Source Index (landed, 2026-07-15)

- Translation layer: `apps/acowork-desktop/src-tauri/src/commands/chat_mqtt.rs:672-706` (StreamDelta/RecordComplete proto→JSON translation)
- Store — the ADR-035 single buffer: `apps/acowork-desktop/src/stores/chatStore.ts:37-66` (the `activeStreams` Map, `ActiveStream`/`StreamLine` types, `getStreamingContent` prefers activeStream)
- Store — event writing: `apps/acowork-desktop/src/stores/chatStore.ts` the `stream_delta`/`record_complete` cases in `handleMessageEvent` (upsert shell message→lines accumulate→freeze into messages[])
- Store — stopping the old triggers: the `new_data_available` case no longer calls `notifyNewData`; `session_state_changed→idle` changed to a full backstop (`incremental=false`) replacing incremental polling
- agentStore: `apps/acowork-desktop/src/stores/agentStore.ts:620` (`switchSession` removes the `enable_notify`/`disable_notify` invoke calls)
- Rendering D4 (thinking): `apps/acowork-desktop/src/components/chat/ThinkBlock.tsx` (removes `tailContent` + auto-scroll, renders the last 5 lines directly, fixed height with overflow hidden and no scrolling)
- Rendering D5 (assistant): `apps/acowork-desktop/src/components/chat/MessageBubble.tsx:279` (`isStreaming` early-returns to the processing animation without rendering text)
- Cleanup: `clearSessionStreaming` merged into `activeStreams.delete(sessionId)`

### Phase 2.5 Implementation Source Index (landed, 2026-07-15)

- Backend HTTP truncation: `core/acowork-runtime/src/cli.rs:2917` (`truncate_tool_result_for_display`, called on all 3 HTTP paths)
- Backend MQTT truncation: `core/acowork-runtime/src/mqtt/client.rs:520` (`publish_tool_result` calls `truncate_tool_result_lines`, `:908` is the function definition)
- Frontend truncation removed: `apps/acowork-desktop/src/components/chat/ExploreBlock.tsx:582` (the 500-character second truncation deleted; the backend has already trimmed)
