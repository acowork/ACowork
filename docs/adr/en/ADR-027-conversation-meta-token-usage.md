# ADR-027: Cumulative Token Consumption Statistics in Conversation Meta

> **Chinese source of truth**: [ADR-027](../zh/ADR-027-conversation-meta-token-usage.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft

## Date

2026-07-07

## Decision Makers

大鱼 (Dayu)

## Blast radius

- `core/acowork-runtime/src/conversation.rs` — the core change: the `SessionMeta` struct, the `ConversationSession` accumulation methods, the `build_meta` / `resume` paths
- `core/acowork-runtime/src/agent/loop_context.rs` — the main AgentLoop call site
- `core/acowork-runtime/src/episode_distill.rs` — the `compact_with_llm` / `compact_session_title_with_llm` signature change
- `core/acowork-runtime/src/agent/loop_.rs` — the title generation caller
- `core/acowork-runtime/src/agent/history.rs` — the compaction caller
- `core/acowork-runtime/src/agent/loop_session.rs` — the session-end distillation caller
- `core/acowork-runtime/src/agent/session_state.rs` — optional: `SessionStateSnapshot` extension
- `core/acowork-runtime/src/agent/session/session_task.rs` — the initial ContextUsage uses the new fields
- `core/acowork-runtime/src/providers/anthropic.rs` — added 2026-09-21: `merge_prompt_usage` — a zero-skipped input must be merged across events, see "Merging usage across events"

---

## Context

`SessionMeta` currently keeps only a **snapshot of a single LLM call**:

```json
{
  "last_input_tokens": 47456,
  "last_output_tokens": 505
}
```

These fields are written by `ConversationSession::update_last_tokens()` after every LLM
response. `input_tokens` falls back to a local **char-based estimate** when
`prompt_tokens_reliable=false` (the Provider returned `prompt_tokens=0`), which violates the
user's explicit requirement of "do not use estimates in statistics".

The existing `last_input_tokens` / `last_output_tokens` record only the **most recent** call,
so the user cannot see how many tokens a session consumed cumulatively.

**User requirements**: (1) record the **cumulative** token total for the whole session in the
meta JSON; (2) count input and output separately (`total_input` / `total_output`); (3) use
only the real values returned by the LLM API, **never estimates**; (4) cover every LLM call
in the session (main interaction + compaction + title generation + episode distill); (5) no
backward compatibility with old file formats, since the project is still in development.

**Goals**: cumulative rather than snapshot; 100% real values (input accumulates only when
`usage.prompt_tokens > 0`, never a local estimate); full coverage; and a unified nested
`tokens` object replacing the flat `last_input_tokens` / `last_output_tokens`.

## Design

### Data model

```rust
/// Per-session LLM token usage statistics.
///
/// ADR-027: All values derived from LLM API ground truth (`UsageInfo` from
/// `ChatResponse.usage`).  Provider estimates are NEVER accumulated; iterations
/// where the Provider returns `prompt_tokens = 0` (or no usage) are skipped
/// for `total_input` to preserve accuracy.
///
/// `last_input` / `last_output` always record the most recent call's raw
/// values (including zero) — they serve the same purpose as the legacy
/// `last_input_tokens` / `last_output_tokens` fields they replace.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionTokens {
    /// Snapshot of the most recent single-call prompt/input tokens.
    pub last_input: u64,
    /// Snapshot of the most recent single-call completion/output tokens.
    pub last_output: u64,
    /// Cumulative input tokens across all session LLM calls.
    /// Only accumulates when `usage.prompt_tokens > 0`.
    pub total_input: u64,
    /// Cumulative output tokens across all session LLM calls.
    pub total_output: u64,
}
```

### `SessionMeta` changes

`SessionMeta` keeps its version / session_id / agent_id / created_at identity fields, its
user/API-mutable fields (`title`, `workspace_id`, `model`, `provider`, `reasoning_effort`,
`temperature`), its runtime statistics (`message_count`, `last_active_at`) and its
`last_compaction_offset` / `corrupted` markers. The change is:

- **removed**: the flat `last_input_tokens` / `last_output_tokens` fields
- **added**: `tokens: Option<SessionTokens>` (`None` = no LLM call has been recorded yet)
- `version` bumps to 2

```json
{
  "version": 2,
  "session_id": "20260706_092158_6005e7",
  "agent_id": "com.acowork.senior-engineer",
  "created_at": "2026-07-06T01:21:58.465Z",
  "title": "some topics",
  "model": "gpt-4o",
  "provider": "openai",
  "message_count": 118,
  "last_active_at": "2026-07-06T01:55:25.114Z",
  "tokens": {
    "last_input": 47456,
    "last_output": 505,
    "total_input": 2384901,
    "total_output": 128340
  },
  "last_compaction_offset": 4096,
  "corrupted": false
}
```

### `ConversationSession` changes

A new `tokens: std::sync::Mutex<Option<SessionTokens>>` field replaces `last_tokens`, and
`accumulate_llm_usage` replaces `update_last_tokens`:

```rust
impl ConversationSession {
    /// Record LLM usage from a Provider response and accumulate into totals.
    ///
    /// ## Accuracy guarantee
    ///
    /// - `total_input` only accumulates when `usage.prompt_tokens > 0` —
    ///   Providers that return 0 (or omit usage entirely) are skipped
    ///   to avoid polluting the cumulative counter with local estimates.
    /// - `total_output` always accumulates (completion tokens are always
    ///   real and never fall back to estimation).
    /// - `last_input` / `last_output` are always recorded from the raw
    ///   Provider values (even zero) for the snapshot use case.
    pub fn accumulate_llm_usage(&self, usage: &UsageInfo) {
        let prompt = usage.prompt_tokens;
        let completion = usage.completion_tokens;

        if let Ok(mut guard) = self.tokens.lock() {
            let t = guard.get_or_insert(SessionTokens::default());
            t.last_input = prompt;
            t.last_output = completion;
            if prompt > 0 {
                t.total_input = t.total_input.saturating_add(prompt);
            }
            t.total_output = t.total_output.saturating_add(completion);
        }
        self.write_meta();
    }
}
```

### Accumulation points

| # | Call site | File | Current state | Change |
|---|--------|------|----------|------|
| 1 | Main AgentLoop, after each LLM response | `loop_context.rs:689` | already calls `update_last_tokens(ctx_usage.*)` | call `accumulate_llm_usage(usage)` instead (using the raw `usage`, not `ctx_usage`) |
| 2 | After a compaction call | `episode_distill.rs:compact_with_llm` | returns `Result<String>`, discarding `response.usage` | return `Result<(String, UsageInfo)>`; the caller accumulates |
| 3 | After title generation | `episode_distill.rs:compact_session_title_with_llm` | same | same |
| 4 | Session-end distillation | `episode_distill.rs:distill_on_session_end` | calls `compact_with_llm` but discards the usage | call `accumulate_llm_usage` once the `UsageInfo` is in hand |

**The key change: the `compact_with_llm` / `compact_session_title_with_llm` signatures**
both go from returning `Result<String>` to returning `Result<(String, UsageInfo)>`. The
caller adapts like this:

```rust
// after the change — title generation in loop_.rs
tokio::spawn(async move {
    match compact_session_title_with_llm(&prompt, provider.as_ref(), &model, max).await {
        Ok((title, usage)) => {
            // the original title-setting logic
            if let Some(ref conv) = conversation {
                conv.accumulate_llm_usage(&usage);
            }
        }
        Err(e) => tracing::warn!(...)
    }
});
```

### Merging usage across events (the provider adapter layer)

The "zero-skip" rule (`accumulate only when prompt_tokens > 0`) can only guarantee that **no
false number is recorded**; it cannot guarantee the Provider's real value actually reaches
this point. `UsageInfo.prompt_tokens` is assembled by each provider parser from stream
events, and **where the prompt count lives is not uniform across the Anthropic-compatible
ecosystem** — the upstream SDK types themselves permit two locations:

| Event | SDK type | Prompt-related fields | Semantics |
|---|---|---|---|
| `message_start.message.usage` | `Usage` | `input_tokens` **required**; `cache_creation_input_tokens` / `cache_read_input_tokens` optional | the input count for a single call (the spec's default location) |
| `message_delta.usage` | `MessageDeltaUsage` | `input_tokens` / both cache fields **optional**; `output_tokens` required | **cumulative** |

The official wording on `message_delta.usage`:

> Total input tokens in a request is the summation of `input_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens`.

Sources: the Anthropic SDK `usage.py`, `message_delta_usage.py` and
`raw_message_delta_event.py`.

Two spec facts determine the adapter's implementation:

1. **`message_delta` carrying prompt counts is legitimate**, not a private extension — so the prompt count must not be hard-bound to `message_start`.
2. That report is **cumulative** — so within one request, taking "the largest non-zero report" suffices and no summation is needed.

`anthropic.rs::merge_prompt_usage` applies the same single rule to **every** event that
carries usage:

```rust
let input = usage.input_tokens.unwrap_or(0) + cache_read + cache_write; // the official sum formula
if input > *input_tokens {                                             // the largest non-zero report wins
    *input_tokens = input;
    *cache_read_tokens = cache_read;    // cache and total come from the same report, staying consistent
    *cache_write_tokens = cache_write;
}
```

Taking `max` rather than "last writer wins" or summation, because: 0 is a legal value but
semantically means "not reported", so `max` prevents a 0 from masking a value another event
already reported; an implementation reporting increments (rather than a cumulative total)
yields smaller values, which `max` will not be dragged down by; and the cache counts come
from the same source as the total, so `total_input` can never disagree with the cache counts.

**Triggering case (2026-09-21)**: MiniMax-M3 (provider `minimax-cn-coding-plan`, Anthropic
protocol) sends all-zero usage in `message_start` (`input_tokens: 0`, cache fields `null`),
and the real value only appears on `message_delta`. The old implementation read only
`message_start`, so `prompt_tokens` was 0 on every call, and combined with this ADR's
zero-skip rule `total_input` stalled permanently (while `total_output` kept rising). **The
symptom was not this ADR's rule being wrong, but the upstream always feeding it 0.** The same
case also confirms the cumulative semantics of the delta report: `cache_read_input_tokens`
increased monotonically (32020 → 32648 → 33217 → 33431 → 33695), with each increment exactly
equal to the previous `input_tokens`.

**Residual assumption**: `max` holds on the premise that the prompt count for one request is
monotonically non-decreasing. If some Provider reports **unaccumulated fragment amounts** on
`message_delta` and a fragment exceeds the `message_start` total, the result is too large.
Per spec the delta is cumulative, so this is not handled; tighten it if a counterexample
appears.

**Relation to the cache fields**: the cache counts go through the same merge rule (see
ADR-066). The old implementation only updated the cache counts in the `message_start`
branch, so when a Provider reported cache on `message_delta` the cache would silently reset
to zero alongside — the same bug appearing a second time.

### Accuracy constraint

```mermaid
flowchart TD
    A["Provider returns ChatResponse"] --> B{"usage.prompt_tokens > 0?"}
    B -->|yes| C["tokens.total_input += usage.prompt_tokens"]
    B -->|no (provider gap / fallback path)| D["skip the input accumulation"]
    C --> E["tokens.total_output += usage.completion_tokens"]
    D --> E
    E --> F["tokens.last_input = usage.prompt_tokens"]
    F --> G["tokens.last_output = usage.completion_tokens"]
    G --> H["write_meta()"]
```

### Known gaps

| Provider scenario | usage.prompt_tokens | Handling |
|---|---|---|
| OpenAI normal streaming (`stream_options.include_usage=true`) | > 0 | ✅ accumulates normally |
| OpenAI fallback 1 (strips `stream_options`) | no usage returned | ⏭ skip input; output is also absent so it is skipped too (output is missing together with input) |
| OpenAI fallback 2/3 (further degraded) | no usage returned | same |
| Anthropic normal (`message_start` + `message_delta`) | > 0 | ✅ accumulates normally |
| An Anthropic-compatible implementation reports prompt counts only on `message_delta` (e.g. MiniMax-M3, where `message_start` is all zeros) | > 0 (after the cross-event merge) | ✅ accumulates normally, see "Merging usage across events" |
| Ollama normal streaming | may be 0 (`prompt_eval_count` missing) | ⏭ skip input; accumulate output if `eval_count` is present |
| Local Provider / mock data | may be 0 | ⏭ skip |

**Core principle**: prefer missing over estimating. Missing usage cannot cause a wrong
number — the next normal LLM call keeps accumulating.

### The `resume` path and concurrency

`resume` reads `tokens` from the meta JSON (`tokens: std::sync::Mutex::new(meta.tokens)`),
and subsequent `accumulate_llm_usage` calls do `saturating_add` on top of that base. The
`std::sync::Mutex<Option<SessionTokens>>` guard matches the existing `last_tokens` field:
since `ConversationSession` is `Send + Sync` (shared via `Arc` in async contexts) and
`accumulate_llm_usage` is the only writer (called at the end of each LLM response), there is
no high-contention contention. The `temp + rename` in `write_meta()` guarantees atomic
writes.

### Frontend exposure (Phase 2, recorded in the ADR)

The frontend currently receives a single-call token snapshot through
`ChunkEvent::ContextUsage`, shown in the ResultPanel and the ContextUsageIcon. The
cumulative values can be exposed via **Option A (recommended)**: extend
`SessionStateSnapshot` with `tokens_total_input` / `tokens_total_output`, read from
`conversation.tokens()` in `emit_session_state()` and delivered to the frontend by the HTTP
pull API or a `session_state_changed` event. **Option B**: the frontend reads them from the
`list_sessions` response, since `scan_sessions_from_meta()` already returns the full
`SessionMeta` including `tokens`, which works for a list page. **Option C**: add a
`ChunkEvent::TokenUsage` variant pushing the cumulative value after each accumulation.

## Implementation plan

**Phase 1 — data model + core interface (~+80 / -20 lines)**: define the `SessionTokens`
struct; remove `last_input_tokens` / `last_output_tokens` from `SessionMeta` and add
`tokens: Option<SessionTokens>`; add the `ConversationSession.tokens` field and
`accumulate_llm_usage()`; remove the `last_tokens` field and `update_last_tokens()`; update
`build_meta()` to use `tokens`; update `resume()` to read from `meta.tokens`; rename the
`last_tokens()` getter to `tokens()` returning `Option<SessionTokens>`.

**Phase 2 — main AgentLoop call site (~+5 / -10 lines)**: `loop_context.rs:689` switches from
`ctx_usage.*` + `usage.*` to `accumulate_llm_usage(&usage)` (the raw API values).

**Phase 3 — compaction / title / distill call sites (~+30 / -5 lines)**:
`compact_with_llm` and `compact_session_title_with_llm` return `(String, UsageInfo)` and the
callers adapt; `distill_on_session_end`, the title generation in `loop_.rs` and the
compaction in `history.rs` all accumulate afterwards.

**Phase 4 — tests (~+50 lines)**: unit tests for `accumulate_llm_usage` (reliable /
unreliable / missing usage); `SessionMeta` serialization tests; a compile check that the
compaction call sites were adapted.

**Phase 5 (optional) — frontend display**: `SessionStateSnapshot` gains
`tokens_total_input` / `tokens_total_output`, `emit_session_state()` fills them, and the
frontend ResultPanel displays the cumulative values.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A Provider repeatedly returns `prompt_tokens=0` → the input total stays zero | the user sees an input total of 0 despite real conversation | the missing input total is visible while output is normal; this is a Provider-side problem and the statistics system must not paper over it |
| A compaction LLM call fails (timeout / error) | one call's tokens are lost | does not affect later accumulation; a failure writes no usage and the next normal call makes up for it |
| The `compact_with_llm` signature change spreads | multiple callers need updating | it only touches `history.rs:compact_via_llm` / `loop_.rs:title` / `episode_distill`; the IDE catches every compile error |
| `write_meta` does disk I/O in the high-frequency LLM loop | theoretical performance impact | LLM calls happen on a seconds-scale frequency and `write_meta` writes 400 B, so the cost is negligible — far below the `append_message` hot path |

## Alternatives compared

### A — a nested `tokens` object (**adopted**)

**Advantages**: the semantic layering is clear (`tokens` is one complete "token statistics"
concept); it is extensible later (`tokens.cache_read` / `tokens.reasoning_tokens` /
`tokens.last_active_at`); and it does not get confused with flat field names. **Disadvantage**:
one more level of JSON nesting.

### B — flat fields

**Rejected**: `last_input_tokens` and `total_input_tokens` are confusingly similar; extending
with cache/reasoning fields later would require either more flat names
(`total_cache_read_tokens`) or renaming in a breaking way; and flat fields at the same level do
not group semantically.

## Decision

1. **Adopt the nested `tokens: Option<SessionTokens>` structure**
2. **Remove the top-level `last_input_tokens` / `last_output_tokens` fields**
3. **Accumulate only real values**: input accumulates when `usage.prompt_tokens > 0`; output always accumulates
4. **Cover every LLM call in the session**: the main loop + compaction + title + distill
5. **`compact_with_llm` / `compact_session_title_with_llm` return `(String, UsageInfo)`**
6. **No backward compatibility is kept**: the `last_input_tokens` / `last_output_tokens` fields in old meta JSON files are ignored by the new version (`#[serde(default)]` behavior)
7. **The provider adapter layer must merge usage across events**: the prompt count (including cache) takes the largest non-zero report in the stream; it must not be bound to a single event position — otherwise, when the upstream feeds 0, this ADR's rule will correctly "protect" a wrong number (see "Merging usage across events")

## Changelog

| Date | Revision |
|------|------|
| 2026-09-21 | Added "Merging usage across events (the provider adapter layer)": the Anthropic spec basis (`MessageDeltaUsage`'s prompt fields are legitimate and cumulative), the `merge_prompt_usage` rule and its residual assumption; a MiniMax-M3 row in "Known gaps"; decision item 7 appended |
