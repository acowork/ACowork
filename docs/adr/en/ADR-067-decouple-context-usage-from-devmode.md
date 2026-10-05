# ADR-067: Decouple Context Usage Section Sizes from DevMode

> **Chinese source of truth**: [ADR-067](../zh/ADR-067-decouple-context-usage-from-devmode.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented

## Date

2026-09-04

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-048](./ADR-048-debug-protocol-mqtt-http.md) — DevMode debug panel
- [ADR-060](./ADR-060-prompt-cache-friendly-context-block-reorg.md) — context block reorg
- [ADR-066](./ADR-066-llm-provider-cache-tokens.md) — cache token accumulation

---

## Context

The `ContextUsageIcon` next to the input box shows a popover breaking the context into
five categories (system / tools / messages / connectors / skills), each as a percentage.
The percentages come from section byte sizes divided by total bytes, weighted by the
`usage_percent` the LLM actually reports.

Before this ADR those byte sizes reached the UI through exactly one channel:
`DebugObserverImpl::on_context_built`, which only fires under
`DebugObserverSlot::Dev` — that is, only when the DevMode panel is open. In Production
the slot is a no-op wrapper.

**So unless the user opened the Debug panel, all five percentages stayed at 0**, while the
outer ring `usage_percent` was correct because it travels on a separate always-on
`ContextUsage` chunk push. The frontend read sections out of `useDebugStore`
snapshots array, mirroring the same failure.

The report that started this: the five sub-items in the popover all showed 0 until the
Debug panel was opened. Three facts establish the scope:

- **This is UI state, not debug information.** The context usage ring and its breakdown are
  global input-box UI, a separate display surface from the Debug panel. Data appearing only
  when Debug is on is a plain coupling misplacement.
- **The runtime already has the data on the wire.** `ContextUsageInfo` is pushed to
  chatStore after every LLM call via `ChunkEvent::ContextUsage` — the same path that
  carries `usage_percent`. What is missing is the `sections` field in that same payload.
- **The runtime logic does not need DevMode.** `on_context_built` is effectively a pure
  function of a `ContextBuilder`, a `HistoryManager`, the MCP tools list, and the model
  name. `DebugObserverSlot::Production` disabled it only because DevMode was thought to be
  its sole consumer.

## Decision

Move section byte production off the debug-snapshot channel and onto the runtime
observability channel, so the always-on `ContextUsageInfo` payload carries `sections` itself.

**1. Extract `compute_section_sizes` as a free function.** Move the byte-accumulation loop
out of `DebugObserverImpl::on_context_built` into a top-level
`pub fn compute_section_sizes(builder, history, mcp_tools, model) -> Vec<ContextUsageSection>`
in `agent/context.rs`. **This is the single implementation source** — both the DevMode path
and the always-on path MUST use it, so the section key order and byte counts the UI sees
are identical.

`todo_context` is deliberately **not** a separate section: ADR-060 v2 moved the todo
snapshot (Block C) out of the system prompt, so todo state exists only inside the
`todo_write` tool results in history, which are already counted in `messages`. A separate
`todo_context` section would double-count. The `latest_todo_write_content` scan helper is
deleted along with it.

**2. Add `sections` to `ContextUsageInfo`.**

```rust
pub struct ContextUsageSection {
    pub key: String,        // stable contract: "system_prompt" / "messages" / ...
    pub size_bytes: u64,     // exact UTF-8 byte count
}

pub struct ContextUsageInfo {
    // ...existing fields...
    pub sections: Option<Vec<ContextUsageSection>>,
}
```

It is `Option<Vec<…>>` rather than a bare `Vec` for forward compatibility: an un-upgraded
Runtime leaves it unset, and the frontend `contextUsage.sections ?? []` degrades naturally to
"no data" (the popover shows zeros, which is known and does not crash).

**3. Populate `sections` in `process_llm_response_usage`.** In `loop_context.rs`, after
building `ctx_usage` and calling `patch_session_totals`, call
`compute_section_sizes(context_builder, &history, &mcp_tools, current_model)` and attach the
result as `ctx_usage.sections`.

The MCP tools come from `self.core.mcp_tools` — the same set injected into
`ChatRequest.tools` by `build_chat_request` — and **not** from `all_tools` filtered by the
`mcp_` prefix: `all_tools` also contains the built-in `mcp_install` / `mcp_uninstall`, so a
prefix filter would count those two as well and inflate the `tools` category. The
DevMode path receives the same set through `ContextSnapshotRequest::mcp_tools`, keeping both
paths byte-identical.

The DevMode path changes in turn: `DebugObserverImpl::on_context_built` takes
`compute_section_sizes` as its `base_sections` and layers only DevMode-specific
metadata on top (full content, `token_estimate`, SHA256 hash). Byte sizes **always** come
from `base_sections`, so two independent algorithms cannot drift.

**4. Stop `session_state` snapshots from clearing `sections`.** `emit_session_state`
(the retained topic, pushed multiple times per turn) builds context usage via
`build_context_usage_from_persisted`, which has no `ContextBuilder` and so cannot recompute
sections. Left alone it would overwrite `sections` with empty on every arrival, pinning the
popover at 0. Two layers of defence:

- **Runtime**: `process_llm_response_usage` caches the full payload on the
  `ConversationSession` (`cache_context_usage`); `emit_session_state` merges the cached
  sections (`last_context_usage_json`) when it builds its payload.
- **Frontend**: `chatStore` keeps old sections when a `session_state` / `fetchSessionState` update
  supplies a new `contextUsage` without them (`mergeContextUsage`), so categories survive even
  against an older Runtime.

**5. O(1) incremental `messages` byte count.** `compute_section_sizes` no longer serializes the
whole history every turn. `HistoryManager` gains a `messages_json_bytes` counter whose value is
always equal to `serde_json::to_string(&messages).len()` (brackets included):

- `append` / `extend` — O(1) incremental (the serialized length of the entry plus separators)
- `load_restored` / `clear` / `truncate_to` / `replace_middle_with_summary` / the 8-level
  compression / `abandon_tool_result` / `retrieve_tool_result` — recompute or adjust (low frequency)
- reads — O(1)

The default non-debug path therefore pays only one small `tool_definitions`
serialization plus a few `.len()` calls per LLM call, and never touches messages.

## Trade-offs

**Why not keep a separate DevMode byte accumulator?** A split implementation (DevMode adds
hash and `token_estimate`, always-on only needs `size_bytes`) was considered and
rejected: the frontend consumes a single section list and computes percentages from byte
sizes, so a second implementation is pure drift risk — exactly the failure seen when a new
section field was added and only one path was updated. One shared function removes the
split at the root.

**Why not have the frontend subscribe to an `onContextBuilt` MQTT event?** That is a
dev-only channel with no events at all in Production. Subscribing to it would require
adding a new always-on topic, whereas every LLM call already pushes a `ContextUsage` chunk
— adding one field to that payload is the smaller change.

**Why `Option<Vec<…>>` rather than `Vec<…>`?** Forward compatibility. The frontend falls back
with `?? []`, and older construction sites (fallback paths, the
`build_context_usage_from_persisted` resume path, the `tests/context_usage_cache_e2e.rs`
fixture) fill `None` for now. Downgrading to `Vec<…>` once every push path calls `compute_section_sizes`
is a later cleanup, not a blocker here.

## Impact

**User-visible**: the five category percentages in the popover now update continuously
alongside the ring and no longer depend on the Debug panel toggle.

**Performance**: `compute_section_sizes` runs once per LLM call. Its heaviest part (history
serialization) is removed by the `messages_json_bytes` counter, leaving one small
`tool_definitions` serialization (built-in plus MCP tool schemas, typically < 50KB) and a few
`.len()` calls — under 0.1ms, negligible. DevMode still serializes messages once for the
panel (hash, token estimate, lazy content), which is a developer-tool cost off the hot path.

**Wire**: `ContextUsageInfo` gains one field, roughly 200–400 bytes depending on the
number of sections, at unchanged chunk frequency (once per LLM call).

**Impact**

| File | Change |
|------|--------|
| `acowork-core/src/protocol.rs` | `ContextUsageSection`; `ContextUsageInfo.sections: Option<Vec<…>>` |
| `acowork-runtime/src/agent/history.rs` | `messages_json_bytes` incremental counter, maintained O(1) on `append`/`extend` and recomputed on structural operations; 3 unit tests |
| `acowork-runtime/src/agent/context.rs` | new `compute_section_sizes`; `messages` section uses `HistoryManager::messages_json_bytes()`; **remove** the `todo_context` section and the `latest_todo_write_content` helper; 2 unit tests |
| `acowork-runtime/src/agent/loop_context.rs` | `process_llm_response_usage` fills `ctx_usage.sections` and caches the payload; MCP tools from `self.core.mcp_tools` |
| `acowork-runtime/src/agent/loop_session.rs` | `emit_session_state` merges cached sections so a retained snapshot cannot zero the popover |
| `acowork-runtime/src/conversation.rs` | `cache_context_usage` / `last_context_usage_json` |
| `acowork-runtime/src/debug/observer.rs` | `ContextSnapshotRequest` gains `mcp_tools` |
| `acowork-runtime/src/debug/observer_impl.rs` | `on_context_built` uses `compute_section_sizes` as its only source and consumes `req.mcp_tools` |
| `acowork-runtime/tests/context_usage_cache_e2e.rs` | every `ContextUsageInfo` construction site adds `sections: None` |
| `apps/acowork-desktop/src/lib/types.ts` | `ContextUsageInfo.sections?: ContextUsageSection[]` |
| `components/chat/ContextUsageIcon.tsx` | drop the `useDebugStore` dependency; read `chatStore.contextUsage.sections` |
| `stores/chatStore.ts` | `mergeContextUsage` preserves old `sections` when a new value omits them |
| `ContextUsageIcon.test.tsx`, `chatStore.test.ts` | remove the debug mocks; add the ADR-067 regression tests (2 store tests) |

## Follow-up cleanup (out of scope)

- `ContextUsageInfo.sections: Option<Vec<…>>` → `Vec<…>`; the cache plus frontend merge already guarantee
  `session_state` carries sections, so the Option can be dropped when convenient.
- The `tool_defs_str` serialization inside `DebugObserverImpl::on_context_built` could reuse
  `compute_section_sizes` instead — but the DevMode panel still needs full content, so it keeps its
  own stringification for now.

## Verification

- Rust: `history::tests::messages_json_bytes_*` (3 — incremental vs full-serialization parity, clear/truncate, abandon/retrieve) and `context::tests::compute_section_sizes_*` (2) pass;
  `cargo test -p acowork-runtime --lib -- --test-threads=1` 1345 pass.
- Frontend: `ContextUsageIcon.test.tsx` 6, `contextUsageBreakdown.test.ts` 12, and
  `chatStore.test.ts` 38 (including the new session_state sections-preservation regression) pass;
  `cacheHitRate.test.ts` 46 pass.
