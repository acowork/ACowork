# ADR-028: Process-Level Accumulated Token Usage Cache in AgentCore

> **Chinese source of truth**: [ADR-028](../zh/ADR-028-agent-core-token-usage-cache.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

In progress

## Date

2026-07-16

## Decision Makers

大鱼 (Dayu)

---

## Context

ADR-027 added `tokens.total_input` / `tokens.total_output` to each session’s
`SessionMeta`, recording cumulative LLM usage per session. Two gaps remain:

1. **No agent-level view** — users need the total across all sessions of an agent, and
   the frontend cannot compute it (it would have to walk every session meta file).
2. **A startup blind window** — the first `ContextUsageInfo` push only fires after the
   first LLM call, so a freshly started Runtime shows nothing in the Results Panel.

Requirements: show process-level input/output totals in the Agent Status panel; show
historical totals even with no active session or LLM call; update live after each call;
and **do not persist to disk**, to avoid a consistency problem with the `SessionMeta`
fields.

## Decision

Add two `AtomicU64` counters to `AgentCore` and expose them through two data
sources, live taking precedence.

### Data model

```rust
// core/acowork-runtime/src/agent/agent_core.rs
pub(crate) agent_total_input_tokens: AtomicU64,
pub(crate) agent_total_output_tokens: AtomicU64,
```

`ContextUsageInfo` (protocol.rs) and `SessionsListResponse` (Gateway `chat.rs`) both gain
`agent_total_input_tokens` / `agent_total_output_tokens` as `Option<u64>`;
the frontend `AgentStorage` gains `agentTokenTotals: { input, output } | null`.

### Two data sources

| Source | Trigger | Content |
|--------|---------|---------|
| **Primary (live)** | every `ContextUsageInfo` push | a snapshot of the `AgentCore` atomic counters, real time within the current Runtime process |
| **Fallback (session list)** | `GET /api/agents/:id/sessions` | a full disk scan of every session meta, aggregated by `scan_sessions_async` — used when the Runtime just started and no LLM call has happened |

Frontend precedence: once the live value exists (after the first LLM call) it always wins;
otherwise it falls back to the session-list value; with neither, the row is hidden or
shown as `—`.

### Counter update flow

```
LLM call (4 call sites)
    ├── ConversationSession::accumulate_llm_usage(usage)   → SessionMeta
    └── AgentCore::accumulate_llm_usage(usage)              → atomic add
              │
              ▼
ContextUsageInfo push (3 push sites) — carries the counter snapshot
              ▼
          WebSocket → Frontend Results Panel (Agent Status)

GET /api/agents/:id/sessions
    ├── scan_sessions_async → (total_in, total_out)
    ├── AgentCore::merge_token_totals((in, out))            → atomic max
    └── response → frontend agentStore stash → Results Panel fallback
```

### Why atomic max merge

`merge_token_totals` uses `fetch_update` to apply `counter = max(counter, scanned)`:

| Ordering | Result | Verdict |
|----------|--------|---------|
| merge, then accumulate (scanned=1000, +10) | 1010 | the counter catches up |
| accumulate, then merge (+10 → counter=10, scanned=1000) | 1000 | max keeps the scanned history |
| concurrent | 1010 | one call is masked by the merge, but the next accumulate restores it |
| repeat merge (idempotent) | 1000, unchanged | merge is idempotent |

**Max semantics never drop a non-zero LLM call under any interleaving.** At worst the
last call inside a short window is masked, and the next push or scan corrects it.

### Why there is no startup seed

Seeding the counters from disk at Runtime startup was rejected:

1. **Duplicate scanning** — `handle_list_sessions` is on the session-list path and already
   does a full scan plus atomic-max merge on every call, which right after startup is
   usually the first user action, making the seed redundant.
2. **No extra I/O on the cold start** — on-demand scanning hides the latency behind the
   existing session-list load.
3. **Single source of truth** — not seeding avoids a consistency check between
   `SessionMeta` and `AgentCore`.

## Edge cases

- **Process restart** — the counters reset to zero; the next `GET /api/agents/:id/sessions`
  triggers `scan_sessions_async` → `merge_token_totals` and restores the baseline from disk.
  Until then the frontend shows `—`.
- **Concurrent merge vs accumulate** — atomic `fetch_update` keeps the operation atomic and
  max semantics never lose a positive value (see above).
- **Many sessions** — `scan_sessions_from_meta` is O(n) time and O(1) space (accumulators
  only, no session objects built). Acceptable at the expected scale (<10^4 sessions per
  agent).
- **Runtime version compatibility** — both Gateway fields use
  `#[serde(skip_serializing_if = "Option::is_none")]`. An older Runtime omits them, so the
  Gateway `list_sessions` handler reads them with `data.get(...).and_then(|v| v.as_u64())`, sets
  `None` on absence, and omits them on serialize. An older Desktop silently ignores fields it
  does not know.

## Tests

`agent_core.rs` unit tests

1. `accumulate_llm_usage` basic accumulation over two calls
2. `accumulate_llm_usage` skips input when `prompt_tokens = 0`
3. `accumulate_llm_usage` saturating overflow near `u64::MAX` (no panic)
4. `merge_token_totals` behaviour: initial, counter > scanned (keep), counter < scanned (take scanned)
5. Mixed concurrent accumulate and merge

`conversation.rs::scan_sessions_async`

- the returned `agent_totals` equals the sum of `tokens.total_input` / `tokens.total_output`
  across all session metas
- an empty directory returns `(0, 0)`

`proto_bridge.rs` — round-trip for the new fields plus backward compatibility (a `None` field
is omitted from the JSON).

## Impact

Runtime: `agent_core.rs` (counters + `accumulate_llm_usage` / `merge_token_totals` /
`agent_token_totals`), `conversation.rs`, `cli.rs` (`handle_list_sessions` merge and
forward), `agent/session/session_manager.rs` (`core()` accessor), `agent/loop_context.rs`
(4 LLM call sites, 3 ContextUsageInfo push sites), `agent/loop_.rs` (title generation),
`agent/loop_session.rs` (session-end distillation). Core: `protocol.rs`,
`proto/gateway_ipc.proto`, `proto_bridge.rs`. Gateway: `http/chat.rs`, `grpc/dispatch.rs`
(defensive default). Desktop: `lib/types.ts`, `stores/agentStore.ts`,
`components/results/ResultsPanel.tsx`, `i18n/locales/*.json` (5 languages).
