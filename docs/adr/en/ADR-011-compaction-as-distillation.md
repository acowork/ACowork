# ADR-011: Unified Strategy for Context Summarization and Distillation

> **Chinese source of truth**: [ADR-011](../zh/ADR-011-compaction-as-distillation.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Proposed

## Date

2026-05-28

## Decision Makers

Architecture discussion

## Refines

[ADR-010](./ADR-010-context-compression-simplification.md) — specifies the Phase 2
distillation strategy

---

## Context

ADR-010 left Phase 2 ("LLM summary + distillation on trim") unimplemented. That phase
treated summarization and distillation as independent operations, which breaks in two ways:

1. **Distillation on trim is semantically incomplete** — it sees only the trimmed middle
   segment, never the last 2–3 turns, so it cannot know where the conversation ended
   and may record abandoned decisions as long-term memory.
2. **Two calls waste budget** — the summary already consumes the full context and
   produces natural language covering what the distillation JSON was meant to capture,
   and natural language recalls better in Grafeo than structured fields.

## Decision

### The summary is the distillation

One Compact Model call takes the full context — nothing excluded, including the last
2–3 turns — and emits one natural language summary. That single output goes two places:

1. **In memory** — replaces the middle segment, yielding
   `[sys, summary, last 2–3 turns]`.
2. **In Grafeo** — written as the distillation result for cross-session recall.

No second call, and no structured distillation JSON.

The summary overlaps the last 2–3 turns. That redundancy is harmless: it cannot produce
contradictions, and the main LLM reads it correctly.

### Session lifecycle

At session close, compare `last_compaction_line` (the JSONL line where the last compaction
wrote) against the current total line count:

| Situation | Action |
|-----------|--------|
| Last message is the compaction summary, nothing added since | skip — all knowledge is already in Grafeo |
| Conversation continued after the summary, a compaction happened | distill the tail, from `last_compaction_line` to end |
| Never compacted | distill the full JSONL — short by definition, so below the Compact Model window |

### Tail distillation sets no minimum threshold

Raw text is never appended straight to Grafeo; the tail always goes through the LLM. The
tail is bounded (below `context_window` × 80%), raw text would carry chit-chat noise, and
session close is infrequent enough that the extra cost is negligible.

### Memory recall is unchanged

Recall still queries Grafeo only, never JSONL. Grafeo remains the sole entry point for
long-term memory; JSONL exists for the frontend to render history and takes no part in
retrieval.

### The episode layer takes summaries only

The per-turn real-time episode write is removed: JSONL is already the per-message source of
truth, so a per-turn copy in Grafeo is redundant. The only write paths into the episode
layer are the compaction summary and the session-close distillation summary. Offline
consolidation consequently fires only at those points, which matches its Phase 3 premise of
batching completed segments.

## Impact

| File | Change |
|------|--------|
| `agent/history.rs` | add `compact_via_llm()` and `replace_middle_with_summary()` |
| `agent/loop_.rs` | three-stage `trim_history_to_budget()`; at 80% call `compact_via_llm`, replace in-memory history, write the summary to Grafeo asynchronously; drop the separate `distill_on_trim` call |
| `agent/session_state.rs` | add `last_compaction_line: Option<u64>` |
| `conversation.rs` | add `line_count()` |
| `episode_distill.rs` | deprecate `distill_on_trim()`; add `distill_tail(session_path, start_line)` |

Also removes: the distillation JSON schema, distillation on trim, and the per-turn episode
write. Design docs: `03-agent-runtime.md` §2.5, `05-memory.md` §2.

## Consequences

**Upside**

- Compact Model calls are halved.
- Distillation sees the full context, so conclusions from the final turns survive.
- Natural language recalls better in Grafeo than structured JSON.
- One concept instead of two — both layers just "summarize"; Grafeo stores segment summaries only.

**Downside**

- The summary repeats the last 2–3 turns (argued harmless above).
- Session close still costs one extra Compact Model call for the tail.

**Relationship to ADR-010**

This ADR specifies ADR-010 Phase 2 and supersedes its "distillation on trim" wording.
