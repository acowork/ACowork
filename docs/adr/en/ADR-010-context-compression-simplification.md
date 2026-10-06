# ADR-010: Major Simplification of the Context Compaction Strategy

> **Chinese source of truth**: [ADR-010](../zh/ADR-010-context-compression-simplification.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Accepted (Phase 1 and 2 complete: programmatic folding removed, LLM compaction and
distillation unified — specified in [ADR-011](./ADR-011-compaction-as-distillation.md))

## Date

2026-05-28

## Decision Makers

Architecture discussion

---

## Context

The original design compacted context programmatically — routine tool result folding,
Phase 1 content folding, FIFO trimming, eight-level priority trimming of retrieval results.
Those strategies decide *what may be discarded* from proxy metrics (message role,
character position, temporal order) rather than from meaning, and proxy metrics always
fail eventually.

## Key Insight

Three contradictions make programmatic compaction unsound:

1. **The truncation point is uncontrollable** — critical information may sit anywhere, so
   truncation preserves quantity, not quality.
2. **Temporal order ≠ importance** — FIFO discards old messages that may still hold
   unresolved dependencies.
3. **Message role ≠ semantic state** — "does an assistant message exist" cannot tell
   whether that reply actually analysed the tool result; in coding scenarios many
   replies are transitional filler carrying no reusable analysis.

**Code can decide *when* to call an LLM for a summary. It cannot decide *what* to
compact.**

## Decision

```
Normal:  context grows → 70% warn → 80% trigger LLM summary → continues
Error:   overflow / API error → emergency_trim → retry
```

**No programmatic folding steps in between.**

| Stage | Trigger | Behaviour |
|-------|---------|-----------|
| 1: monitor | 70% usage | log only |
| 2: LLM summary | 80% usage | Compact Model summarizes the full context; no folding, no truncation |
| 3: emergency trim | 95% / `ContextOverflow` | `emergency_trim`, keep the last N non-system messages |

**LLM summary design**

1. **Separate Compact Model** — cheaper/faster than the main model, ~$0.02 per call.
2. **Full context as input** — nothing folded, truncated, or preprocessed.
3. **Protect head and tail** — keep the system prompt plus the last 2–3 turns; compact the
   middle segment.
4. **Archive the full history** — retrievable on demand, so nothing is lost permanently.
5. **Quality over cost** — 2–3 cents per summary is dwarfed by the cost of fixing errors
   that folding would introduce.

**Dropped**

| Strategy | Reason |
|----------|--------|
| Routine tool result folding (`fold_tool_results`) | uncontrollable truncation point loses critical information |
| Phase 1 content folding (file/inline + FoldedRef + recall hints) | semantic judgement is unreliable; net benefit negative |
| Eight-level priority trimming of retrieval results | a programmatic priority cannot stand in for semantic relevance |
| `BudgetAllocation` elastic partitioning | history/retrieval coordination belongs in the LLM summary |
| Assistant-message check as an "already analysed" marker | code cannot tell whether a reply genuinely contains analysis |

**Kept**

| Strategy | Reason |
|----------|--------|
| LLM summary | the core mechanism; the only way to understand semantics |
| `emergency_trim` | safety net on API overflow |
| Episode distillation | cross-session knowledge transfer, complementary to the summary |
| Token monitoring + threshold triggering | monitoring is the basis of every decision |
| `sanitize_messages` | message repair (orphans, empty messages) — a fix, not compaction |
| Multi-model routing | summarize with a cheap model to control cost |

## Impact

| File | Change |
|------|--------|
| `agent/history.rs` | drop the routine `fold_tool_results()` trigger; demote `trim_fifo()` to emergency use; keep `emergency_trim()` / `sanitize_messages()` / `estimate_text_tokens()`; add `compact_via_llm()` |
| `agent/loop_.rs` | `trim_history_to_budget()` → 70% warn → 80% compact → 95% emergency; remove the 70% pre-trim |
| `agent/context.rs` | wire `system_prompt_cache` into `ContextBuilder::build()`; remove build-time folding/truncation |
| `token/counter.rs` | keep `BudgetAllocation` but mark deprecated — it no longer feeds the trimming pipeline |
| `memory/manager.rs` | `inject()` → semantic-similarity ordering only; drop the eight-level priority |

Design docs: `03-agent-runtime.md` §3.1, `05-memory.md` §1,
`15-conversation-persistence.md` §1.8 — remove Phase 1 folding and the eight-level
retrieval priority.

## Consequences

**Upside**

- A semantic comprehension task goes back to the LLM, avoiding unreliable programmatic judgement.
- `history.rs` shrinks from ~800 to ~300 lines; many heuristics and boundary conditions disappear.
- Summarizing the full context beats summarizing a folded context by a wide margin.

**Downside**

- More input tokens per summary (160K vs 80K), though the absolute cost stays low
  ($0.02–0.03 per call).
- Without folding, tokens grow faster: compaction fires every 8–10 turns instead of 12–15.
- The normal path leans harder on `emergency_trim` in extreme cases.

**Risk mitigation**

- The Compact Model is a cheap one, so per-call cost stays bounded; summary prompt quality
  is the crux and must state which information to preserve.
- `emergency_trim` keeps 4 non-system messages, so an overflow never loses all context.
- The full-history archive lets anything the summary dropped be retrieved on demand.
