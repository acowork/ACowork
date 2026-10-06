# ADR-066: Pass-Through and Cumulative Accounting of LLM Provider Cache Tokens

> **Chinese source of truth**: [ADR-066](../zh/ADR-066-llm-provider-cache-tokens.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

In progress

## Date

2026-07-21

## Decision Makers

大鱼 (Dayu)

## Predecessors

ADR-027 (SessionTokens per-session accumulation), ADR-028 (AgentCore process-level accumulation)

**Blast radius**:

- `core/acowork-core/src/providers/traits.rs` (`UsageInfo`'s fields already exist; this ADR does not
  change them)
- `core/acowork-runtime/src/conversation.rs` (`SessionTokens` +4 fields, the accumulation methods,
  `scan_sessions_async` aggregation)
- `core/acowork-runtime/src/agent/agent_core.rs` (+2 `AtomicU64`, extended
  `accumulate_llm_usage` / `merge_token_totals` / `agent_token_totals`)
- `core/acowork-runtime/src/usecases/agent_token.rs` (the `AgentTokenService` trait's return types)
- `core/acowork-runtime/src/usecases/agent_token_impl.rs` (both impls extended)
- `core/acowork-runtime/src/agent/loop_context.rs` (3 ContextUsage push sites)
- `core/acowork-runtime/src/agent/context.rs` (`compute_context_usage` /
  `build_context_usage_from_persisted`)
- `core/acowork-runtime/src/startup/session_init.rs` (the resume path's `merge_token_totals`)
- `core/acowork-runtime/src/agent/session/session_manager.rs` (`merge_token_totals`)
- `core/acowork-core/src/protocol.rs` (`ContextUsageInfo` +6 fields)
- `core/acowork-runtime/src/usecases/session_metadata.rs` (`SessionsListResponse` +2 fields — see
  "Actual Implementation Deviations")
- `core/acowork-runtime/src/usecases/session_metadata_impl.rs` (`list_sessions` extended in step)
- `core/acowork-runtime/tests/context_usage_cache_e2e.rs` (new e2e test)
- `apps/acowork-desktop/src/lib/types.ts` (`ContextUsageInfo` + `agentTokenTotals` extended)
- `apps/acowork-desktop/src/lib/cacheHitRate.ts` (new — the hit-rate pure-function helper)
- `apps/acowork-desktop/src/lib/cacheHitRate.test.ts` (new — helper unit tests)
- `apps/acowork-desktop/src/stores/agentStore.ts` (`agentTokenTotals` gains the cache dimensions)
- `apps/acowork-desktop/src/components/results/ResultsPanel.tsx` (cache rows + hit rate in the Agent
  Status / Session Status panels)
- `apps/acowork-desktop/src/components/chat/ContextUsageIcon.tsx` (the popover summary gains a cache
  row — see "Actual Implementation Deviations")
- `apps/acowork-desktop/src/i18n/locales/*.json` (i18n keys)

> **Revision note** (during implementation): the original blast radius listed
> `core/acowork-gateway/src/http/chat.rs` and `core/acowork-gateway/src/grpc/dispatch.rs`, but
> `chat.rs` had already moved to `core/acowork-runtime/src/usecases/session_metadata.rs` during
> ADR-040 (the UseCase layer refactor) and was further split into `session_metadata.rs` +
> `session_metadata_impl.rs`; the `dispatch.rs` path no longer exists. The actual implementation
> changed the runtime-side session_metadata files listed above, and the old Gateway paths no longer
> represent the real code locations.

---

## Background

### Native Provider API support

| Provider | Cache Read | Cache Write | Response fields |
|----------|:---------:|:-----------:|-----------------|
| **OpenAI Chat Completions** | ✅ | ❌ (automatic caching, no write distinction) | `usage.prompt_tokens_details.cached_tokens` |
| **Anthropic Messages** | ✅ | ✅ | `usage.cache_read_input_tokens` + `usage.cache_creation_input_tokens` |

Both providers already reserve `cache_read_tokens` / `cache_write_tokens` on
[`UsageInfo`](../../../core/acowork-core/src/providers/traits.rs#L694):

- `providers/openai.rs` `parse_response` / `parse_sse_line` already extract
  `prompt_tokens_details.cached_tokens` into `cache_read_tokens` (`cache_write_tokens` is always 0 —
  OpenAI has no such concept)
- `providers/anthropic.rs` `parse_response` / `parse_anthropic_sse_line` already extract
  `cache_creation_input_tokens` + `cache_read_input_tokens` into `cache_write_tokens` /
  `cache_read_tokens`

**The provider parsing layer is 100% ready and needs no change.**

### Current state (where the chain breaks)

```
Provider response
  │
  ├─ usage.prompt_tokens_details.cached_tokens          (OpenAI)
  ├─ usage.cache_read_input_tokens                      (Anthropic)
  └─ usage.cache_creation_input_tokens                  (Anthropic)
       │
       ▼
  UsageInfo { cache_read_tokens, cache_write_tokens }   ✅ already populated
       │
       ▼ ❌ the chain breaks here
  SessionTokens { last_input, last_output, total_input, total_output }
       │
       ▼ ❌
  AgentCore { agent_total_input_tokens, agent_total_output_tokens }
       │
       ▼ ❌
  ContextUsageInfo { ...no cache fields... }
       │
       ▼ ❌
  Frontend ResultsPanel (no cache rows, no hit rate)
```

| Layer | Location | Problem |
|-------|----------|---------|
| Session accumulation | `conversation.rs::SessionTokens` | only 4 fields; cache_* is dropped |
| Session accumulation | `accumulate_llm_usage` / `accumulate_compaction_usage` | cache_* is not read when building `SessionTokens` |
| Agent accumulation | `agent_core.rs` | only 2 AtomicU64 |
| Agent accumulation | `accumulate_llm_usage` | does not accumulate cache |
| Agent accumulation | `merge_token_totals` / `agent_token_totals` | neither accepts nor returns cache |
| Protocol | `protocol.rs::ContextUsageInfo` | no cache_* fields |
| Push | `loop_context.rs` × 3 | the constructed `ContextUsageInfo` carries no cache |
| Protocol | `agent_token.rs::AgentTokenService` | the trait returns `(u64, u64)`, carrying no cache |
| Persistence merge | `scan_sessions_async` | does not aggregate cache |
| Gateway | `chat.rs::SessionsListResponse` | no `agent_total_cache_*` fields |
| Frontend types | `lib/types.ts::ContextUsageInfo` | no cache_* fields |
| Frontend store | `agentStore.ts::AgentStorage.agentTokenTotals` | only `{input, output}` |
| Frontend UI | `ResultsPanel.tsx` | no cache rows, no hit rate |
| Frontend status bar | `ContextUsageIcon.tsx` | no cache summary |

### User requirements

1. Show the cache tokens (read / write) for the current LLM call and the session cumulative total in
   the Desktop's Session Status panel
2. Compute and show the cache hit rate (to evaluate the cost-effectiveness of Anthropic prompt
   caching and OpenAI auto-caching)
3. The hit-rate baseline must survive a session restart (consistent with ADR-027/028)
4. The process-level Agent Total must support cache accumulation too (consistent with ADR-028)

---

## Goals

1. **Extend `SessionTokens` with 4 fields**: `last_cache_read` / `last_cache_write` /
   `total_cache_read` / `total_cache_write`, all `#[serde(default)]` (backward compatible with old
   v3 meta files)
2. **Extend `AgentCore` with 2 `AtomicU64`**: `agent_total_cache_read_tokens` /
   `agent_total_cache_write_tokens`, following ADR-028's `accumulate_llm_usage` /
   `merge_token_totals` / `agent_token_totals` pattern
3. **Extend the `AgentTokenService` trait's return types** to the 4-tuple
   `(in, out, cache_read, cache_write)`
4. **Extend `ContextUsageInfo` with 6 fields**: per-turn `cache_read_tokens` / `cache_write_tokens`;
   session total `total_cache_read_tokens` / `total_cache_write_tokens`; agent total
   `agent_total_cache_read_tokens` / `agent_total_cache_write_tokens` — all `Option<u64>` +
   `#[serde(default, skip_serializing_if = "Option::is_none")]`
5. **A complete pass-through path**: Provider → `UsageInfo` → `SessionTokens` → `AgentCore` →
   `ContextUsageInfo` → Frontend
6. **Frontend UI**: cache rows + a hit-rate badge in ResultsPanel; a status-bar summary in
   ContextUsageIcon

---

## Solution Design

### 1. Data model

#### `SessionTokens` (extended, `conversation.rs`)

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]  // backward compatible: old v3 meta files without cache fields all default
pub struct SessionTokens {
    pub last_input: u64,
    pub last_output: u64,
    pub total_input: u64,
    pub total_output: u64,
    // ── ADR-066: prompt cache tokens (provider-reported) ───────────────
    /// Last-turn prompt tokens served from cache (OpenAI cached_tokens /
    /// Anthropic cache_read_input_tokens).
    #[serde(default)]
    pub last_cache_read: u64,
    /// Last-turn prompt tokens written to cache (Anthropic
    /// cache_creation_input_tokens; OpenAI has no concept → 0).
    #[serde(default)]
    pub last_cache_write: u64,
    /// Cumulative cache read tokens across all session LLM calls.
    #[serde(default)]
    pub total_cache_read: u64,
    /// Cumulative cache write tokens across all session LLM calls.
    #[serde(default)]
    pub total_cache_write: u64,
}
```

> **No `CONVERSATION_FORMAT_VERSION` bump**: ADR-027 already moved to v3 and this ADR reuses that
> version, with all new fields `#[serde(default)]` so old v3 files deserialize with cache_* = 0 —
> matching the "prefer a miss over an estimate" principle: treating "unrecorded cache" as 0 does not
> pollute the hit-rate denominator (cache_read=0 → hit rate 0%, a sensible fallback).

#### `AgentCore` (extended, `agent_core.rs`)

```rust
pub(crate) agent_total_input_tokens: AtomicU64,
pub(crate) agent_total_output_tokens: AtomicU64,
// ── ADR-066: agent-level cache counters ──────────────────────
pub(crate) agent_total_cache_read_tokens: AtomicU64,
pub(crate) agent_total_cache_write_tokens: AtomicU64,
```

`accumulate_llm_usage` extended:

```rust
pub fn accumulate_llm_usage(&self, usage: &UsageInfo) {
    if usage.prompt_tokens > 0 {
        self.agent_total_input_tokens
            .fetch_update(..., |cur| Some(cur.saturating_add(usage.prompt_tokens)))
            .ok();
        // cache_read follows the same "only accumulate when prompt_tokens > 0" semantics
        // (the provider's fallback zeroes the cache counters too)
        self.agent_total_cache_read_tokens
            .fetch_update(..., |cur| Some(cur.saturating_add(usage.cache_read_tokens)))
            .ok();
    }
    self.agent_total_output_tokens
        .fetch_update(..., |cur| Some(cur.saturating_add(usage.completion_tokens)))
        .ok();
    self.agent_total_cache_write_tokens
        .fetch_update(..., |cur| Some(cur.saturating_add(usage.cache_write_tokens)))
        .ok();
}
```

`merge_token_totals` becomes a 4-tuple (each dimension an independent atomic max), and
`agent_token_totals` returns `(u64, u64, u64, u64)` = `(in, out, cache_read, cache_write)`.

#### `AgentTokenService` trait (`usecases/agent_token.rs`)

```rust
pub trait AgentTokenService: Send + Sync {
    fn accumulate_llm_usage(&self, usage: &UsageInfo);
    fn merge_token_totals(
        &self,
        scanned: (Option<u64>, Option<u64>, Option<u64>, Option<u64>),
    );
    fn agent_token_totals(&self) -> (u64, u64, u64, u64);
}
```

Both implementations (`NoopAgentTokenService` / `InMemoryAgentTokenService`) extend their signatures
in step.

#### `ContextUsageInfo` (`protocol.rs`)

```rust
pub struct ContextUsageInfo {
    // ... existing fields ...
    // ── ADR-066: per-turn cache tokens (from UsageInfo) ──────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    // ── ADR-066: session-total cache tokens ─────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cache_write_tokens: Option<u64>,
    // ── ADR-066: agent-total cache tokens ───────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_total_cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_total_cache_write_tokens: Option<u64>,
}
```

> **Why wrapped in `Option`**: when an old Runtime / frontend without the ADR-066 fields interoperate,
> `#[serde(default, skip_serializing_if = "Option::is_none")]` guarantees the keys are omitted from
> the JSON, so a frontend `undefined` is correctly treated as "not reported" rather than 0.

### 2. The push path

All 3 `ContextUsageInfo` construction sites (`loop_context.rs` etc.) are updated uniformly:

```rust
let (agent_in, agent_out, agent_cache_read, agent_cache_write) =
    self.core.agent_token_totals();
let (total_cache_read, total_cache_write) = session_tokens
    .as_ref()
    .map(|t| (t.total_cache_read, t.total_cache_write))
    .unwrap_or((0, 0));
let ctx_info = ContextUsageInfo {
    // ... existing fields ...
    cache_read_tokens: Some(usage.cache_read_tokens),
    cache_write_tokens: Some(usage.cache_write_tokens),
    total_cache_read_tokens: Some(total_cache_read),
    total_cache_write_tokens: Some(total_cache_write),
    agent_total_cache_read_tokens: Some(agent_cache_read),
    agent_total_cache_write_tokens: Some(agent_cache_write),
};
```

`compute_context_usage` and `build_context_usage_from_persisted` follow the same pattern, preserving
ADR-027's philosophy ("compute looks only at per-turn; the session total is patched by the caller").

### 3. The persistence merge

`scan_sessions_async` accumulates `tokens.total_cache_read` / `tokens.total_cache_write` while
iterating the meta files and returns the 4-tuple `(in, out, cache_read, cache_write)`; the
`list_sessions` handler calls `merge_token_totals(scanned_4_tuple)` with the atomic max merge.

### 4. Gateway and frontend

**`SessionsListResponse`** gains 4 fields at the same level as `agent_total_*`
(2 of them `Option<u64>` + `skip_serializing_if`; see deviation #1 — the cache fields ended up as
required `u64`).

**Frontend `ContextUsageInfo`** (`lib/types.ts`) mirrors the 6 fields.

**Frontend `AgentStorage.agentTokenTotals`** goes from `{ input: number; output: number }` to
`{ input, output, cacheRead, cacheWrite }`.

### 5. UI presentation

#### ResultsPanel (Session Status)

+3 rows in the per-turn area (level with Prompt/Completion): **Cache Read**, **Cache Write**
(both from `UsageInfo`) and **Cache Hit Rate** (derived, see §6). +2 rows in the session total area:
**Total Cache Read** / **Total Cache Write**. +2 rows in the agent total area: **Agent Total Cache
Read** / **Agent Total Cache Write** (falling back to `agentTokenTotals?.cacheRead/Write`).

#### ContextUsageIcon (the chat status bar)

Appended after the existing `... context used` text:

```
[Cache Hit: 64.2% ▮▮▮▮▮▮▯▯▯▯]
```

Shown only when `cache_read_tokens > 0` or `total_cache_read_tokens > 0` (avoiding a misleading 0% in
no-cache scenarios).

### 6. The hit-rate definition

**Two formulas coexist; the frontend switches by provider type**:

| Provider | Formula | Meaning |
|----------|---------|---------|
| **Anthropic** (recommended) | `cache_read / (input_tokens + cache_read + cache_write)` | a write is a precondition for a hit — the more you write, the more you later read |
| **OpenAI** | `cache_read / prompt_tokens` | OpenAI has no write concept |

**Implementation**: the backend does not choose for the frontend, it only passes the raw values
through. The frontend picks the formula by the `sessionProvider` field (already in the session
metadata) when rendering ResultsPanel; `null` (an undefined denominator) hides the badge.

### 7. The overall data flow

```
Provider (OpenAI cached_tokens / Anthropic cache_*_input_tokens)
   │
   ▼
UsageInfo { cache_read_tokens, cache_write_tokens }
   │
   ├── ConversationSession::accumulate_llm_usage(usage)
   │     └── SessionTokens.last_*/total_*  (saturating_add)
   │           └── write_meta() → meta.json on disk
   │
   └── AgentCore::accumulate_llm_usage(usage)
         └── AtomicU64  (saturating_add)
   │
   ▼
ContextUsageInfo push (3 push sites)
   │
   ├── cache_read_tokens/write_tokens           ← per-turn (from usage)
   ├── total_cache_read_tokens/write_tokens     ← session (from SessionTokens)
   └── agent_total_cache_read_tokens/write_tokens ← agent (from AtomicU64)
   │
   ▼ WebSocket / MQTT
   Frontend ContextUsageInfo (typed)
   │
   ├── ResultsPanel: 6 rows + 1 hit-rate row
   └── ContextUsageIcon: status-bar badge

startup / restart:
GET /api/agents/:id/sessions
   → scan_sessions_async accumulates total_cache_* fields
   → AgentCore::merge_token_totals((in, out, cache_read, cache_write))
   → writes back SessionsListResponse.agent_total_cache_*
   → Frontend agentStore.agents[id].agentTokenTotals.cacheRead/Write
```

---

## Edge Cases

### 1. Old v3 meta files (no cache fields)

All `SessionTokens` fields are `#[serde(default)]`, so `total_cache_*` deserializes to 0; the cache
dimension of the hit-rate denominator is 0, the formula returns `null`, and the UI shows nothing — no
display pollution.

### 2. Old Runtime / Desktop interoperability

All new fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`:

- An old Desktop reading a missing field gets `undefined` → the UI falls back to "—"
- JSON from an old Runtime lacks the new fields → the Gateway extracts defensively with
  `data.get(...).and_then(...)`, yielding `None`

### 3. Concurrent accumulate vs merge

Keeps ADR-028's atomic max semantics; the cache dimension behaves identically: a scan may briefly
lag an accumulate, but the next push or scan corrects it immediately.

### 4. The provider does not report cache (no `cached_tokens` field)

Early OpenAI models / Ollama / mocks:

- `cache_read_tokens = 0` / `cache_write_tokens = 0` (`unwrap_or(0)`)
- the hit-rate denominator includes cache → 0, the formula returns `null`, the UI shows nothing
- consistent with ADR-027's "prefer a miss over an estimate"

**Supplement (2026-09-21)**: the above covers "the provider genuinely reports no cache" and must not
be confused with "the provider reported the cache on a different event", which makes cache silently
zero even though the numbers exist. Anthropic's cache fields are legal on both
`message_start.message.usage` and `message_delta.usage`, so they must be **merged across events
first** before deciding whether they are missing — see
[ADR-027 "Usage cross-event merging (provider adapter layer)"](./ADR-027-conversation-meta-token-usage.md).

### 5. OpenAI's `cache_write_tokens` is always 0

By design OpenAI caches automatically and does not distinguish writes, so `last_cache_write` /
`total_cache_write` / `agent_total_cache_write_tokens` are always 0. The UI still shows
"Cache Write: 0" (semantically harmless from OpenAI's perspective and explicitly communicates that
the provider does not support it), or hides the row by provider type (left to the design phase).

### 6. Anthropic's 5min / 1h cache TTL difference

Out of this ADR's scope; the hit-rate definition is unchanged (token-count based, not time-window
based).

---

## Tests

### `conversation.rs` unit tests (extending ADR-027's)

1. **`SessionTokens` serde backward compatibility** — deserializing without cache_* → 0
2. **serde round-trip** — with cache fields, serialize then deserialize identically
3. **`accumulate_llm_usage` accumulates cache** — two calls verify the `total_cache_read/write`
   `saturating_add`
4. **zero-input behaviour** — with `prompt_tokens=0` the cache does not accumulate (consistent with
   input)
5. **`accumulate_compaction_usage` does not pollute `last_cache_*` but accumulates `total_cache_*`**
6. **`set_history_anchor` keeps `total_cache_*` and zeroes `last_cache_*`** — after compaction
   `last_input` is anchored to the post-compaction history size and `last_output=0`, and `last_cache_*`
   is zeroed in step (there is no new per-turn cache snapshot after compaction, until the next
   `accumulate_llm_usage`); `total_cache_*` is kept (the provider-reported true cumulative, unrelated
   to local history estimation)
7. **`scan_sessions_async` aggregates cache** — summing `total_cache_*` across multiple sessions

### `agent_core.rs` unit tests (extending ADR-028's)

1. **`accumulate_llm_usage` accumulates cache** — four calls verify the 4 AtomicU64
2. **saturating overflow** — cache_* near `u64::MAX` does not panic
3. **`merge_token_totals` cache dimension** — the 4-tuple max behaves like ADR-028's in/out
4. **`agent_token_totals` returns a 4-tuple** in the order `(in, out, cache_read, cache_write)`

### `protocol.rs` serialization tests

1. **`ContextUsageInfo` full-field round-trip**
2. **Deserializing without cache fields → `None`**
3. **`skip_serializing_if` behaviour** — `None` fields are absent from the JSON

### `usecases/agent_token.rs` tests

1. **both impls still work after the 4-tuple signature change**
2. **`InMemoryAgentTokenService` accumulates the cache dimensions**

### Gateway `provider_api.rs` tests

1. **`list_sessions`'s response carries the `agent_total_cache_*` fields**
2. **a missing field falls back defensively to `None` in the Gateway**

### Frontend unit tests (vitest)

1. **`computeCacheHitRate(anthropic, ...)`** — the formula is correct
2. **`computeCacheHitRate(openai, ...)`** — the formula is correct
3. **`computeCacheHitRate` with a 0 denominator → `null`**
4. **`ResultsPanel` renders the cache rows + the hit-rate badge** (snapshot test)

---

## Review Comments

(placeholder for the design review record)

---

## Actual Implementation Deviations (recorded during review)

All deviations below were identified during the review phase and either explicitly accepted or
corrected. Each lists what the ADR originally described, what was actually done, and why.

### 1. `SessionsListResponse` field type: `Option` → required `u64`

**Originally**: §4 described `agent_total_cache_*_tokens` as `Option<u64>` +
`#[serde(skip_serializing_if = "Option::is_none")]` for "defensive interoperability with an old
Runtime".

**Actually implemented**: the two cache fields in
`core/acowork-runtime/src/usecases/session_metadata.rs` are required `u64`. The code's comment
justifies itself:

> "Cache fields are emitted unconditionally because the runtime always initialises the agent counters
> (Commit 2 sets both `agent_total_cache_read_tokens` and `agent_total_cache_write_tokens` to `0` on
> every construction site). Desktop frontends that do not yet read these fields stay compatible."

**Rationale**:

- ✅ Simplifies the frontend `AgentStorage.agentTokenTotals` type (no `cacheRead?: number`, just
  `cacheRead: number`; a zero value already means "not reported")
- ✅ The frontend `agentStore.ts` still keeps the `data.agent_total_cache_read_tokens ?? 0`
  defensive fallback when parsing the Gateway response, making it semantically equivalent to
  `Option<u64>` + `skip_serializing_if`
- ⚠️ If a future change to the `AgentCore` constructor forgets to initialise a cache counter, the
  result is an "undefined value" rather than "not reported" — a contract guaranteed by
  `AtomicU64::new(0)`
- Old v3 `SessionTokens` meta files still take the `#[serde(default)]` → 0 path (behaviour unchanged)

**Conclusion**: simplified type + equivalent frontend fallback = accepted deviation. If new agent
counters (cost/discount, etc.) are added later, this ADR must be re-confirmed as to whether "required
+ 0 default" or "Option + skip_serializing_if" is kept.

### 2. ContextUsageIcon status bar: badge → inline text

**Originally**: §5 described appending `[Cache Hit: 64.2% ▮▮▮▮▮▮▯▯▯▯]` after the existing
`... context used` text.

**Actually implemented**: `ContextUsageIcon.tsx` appends a line of text `Cache hit rate 50.0% cached`
inside the popover, not an icon + ASCII progress bar.

**Rationale**:

- ✅ The round icon button (16×16 SVG) has no room for a badge number; the popover is the reasonable
  place
- ✅ "percentage + the word cached" aligns visually with the existing `usage_percent %` row
- ⚠️ Without a progress bar the user cannot intuitively see "full vs not full" — but a cache hit
  rate is a ratio rather than an absolute quantity, so a progress bar's visual metaphor is weak and
  the text suffices

**Conclusion**: UX simplification = accepted deviation. If users later ask for a progress bar, a
mini-bar can be added to the Agent Status panel in ResultsPanel, where there is more space.

### 3. Hit-rate formula: two coexisting formulas → a frontend helper routing by provider

**Originally**: §6 — "Anthropic uses `cache_read / (input + cache_read + cache_write)`; OpenAI uses
`cache_read / prompt_tokens`; both coexist, the frontend switches by provider type".

**Actually implemented**: `apps/acowork-desktop/src/lib/cacheHitRate.ts` routes by protocol family:

- `getCacheProtocol(providerId)` uses an explicit whitelist: `openai` / `azure` / `azure-openai` →
  `"openai"`; `anthropic` / `bedrock` → `"anthropic"`; everything else (including `ollama` /
  `deepseek` / `zhipuai` / `minimax*` / `volcengine-agent-plan` / user-defined OpenAI-compatible
  endpoints) → `null` (hidden)
- `computeCacheHitRate(providerId, usage)` prefers the cumulative `total_cache_read_tokens` and falls
  back to the per-turn `cache_read_tokens`, with the denominator matching the same time dimension
- `formatCacheHitRate(ratio)` emits `12.3%`, clamped to `[0%, 100%]`

**Rationale**:

- ✅ Provider id prefix matching (`startsWith("openai")`) cannot catch every OpenAI-compatible custom
  endpoint; the explicit whitelist refuses to misclassify (misclassification is worse than not
  showing)
- ✅ `cacheHitRate.test.ts` covers 23 cases: protocol classification, the OpenAI formula, the
  Anthropic formula, cumulative preference, Bedrock, the `null` fallback, NaN/Infinity, clamping
- ⚠️ Adding a cache-aware provider requires editing the whitelist + a test — an explicit cost

**Conclusion**: the two-formula implementation fully matches ADR §6's design; "switching by provider
type" is realised by `getCacheProtocol`.

### 4. `compute_context_usage` philosophy: scattered caller patches → a `patch_session_totals` helper

**Originally**: §2 described updating the 3 `ContextUsageInfo` construction sites uniformly, with
the cumulative session fields patched by the caller.

**Actually implemented**: the review found that the 3 push paths (the main push / the context_window
push / the compaction push) each independently inline-patch the 4 cumulative session fields, which
duplicates and risks drift. Refactored into a single helper
`core/acowork-runtime/src/agent/context.rs::patch_session_totals` handling
`total_input_tokens` / `total_output_tokens` / `total_cache_read_tokens` / `total_cache_write_tokens`
uniformly; all 3 push paths call it.

**Rationale**:

- ✅ Centralized patching = a single source of truth, preventing a recurrence of P0 bug 1 (forgetting
  to patch `total_cache_*`)
- ✅ Preserves the existing semantics: cumulative values are still supplied by the caller, per-turn
  values are still filled by `compute_context_usage` / `build_context_usage_from_persisted`
- ✅ `build_context_usage_from_persisted` (the resume path) also goes through the same helper

**Conclusion**: architectural rationalization = accepted refactor.

### 5. End-to-end test coverage filled in

**Originally**: §Tests did not explicitly list an e2e integration test, though §7's data flow diagram
depicted the full "Provider → meta → push → UI" chain.

**Actually implemented** (added during review):
`core/acowork-runtime/tests/context_usage_cache_e2e.rs` covers:

1. the provider response → `ConversationSession::accumulate_llm_usage` → `SessionTokens` persistence
   round trip
2. `AgentCore::accumulate_llm_usage`'s 4-tuple accumulation + the atomic max merge
3. the `patch_session_totals` main push path — verifying the 4 fields
   (`total_cache_read_tokens` etc.) are non-`None`
4. the `ContextUsageInfo` JSON wire format containing the 6 cache fields; old v3 meta compatibility
5. `build_context_usage_from_persisted`'s resume path filling per-turn + cumulative cache

**Conclusion**: the added e2e closes the loop and verifies that P0 bug 1 does not recur.
