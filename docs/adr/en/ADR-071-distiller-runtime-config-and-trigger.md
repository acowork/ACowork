# ADR-071: Memory Distiller Runtime Config and Trigger Wiring (Making EpisodicDistiller Operable)

> **Chinese source of truth**: [ADR-071](../zh/ADR-071-distiller-runtime-config-and-trigger.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented (2026-09, W1–W5 landed; see the work table for commits; W6 documentation)

Revised 2026-10 alongside [ADR-068](../zh/ADR-068-memory-layer-promotion-two-axis-orthogonal.md): the
config surface narrowed, see the revision note at the end.

## Date

2026-09 (revised 2026-10)

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-068](./ADR-068-memory-layer-promotion-two-axis-orthogonal.md) — the two orthogonal
  axes and the EpisodicDistiller engine; this ADR completes its M4/M7 scheduling wiring and
  config surface
- [ADR-063](./ADR-063-package-level-prompt-override.md) — this ADR whitelists the two
  distiller prompts
- [ADR-053](./ADR-053-agent-specific-compaction-prompt.md) — the `summary.md` override precedent
  and the Debug PromptList pattern
- [05-memory.md §4.2](../../design/zh/05-memory.md) — the offline distillation design baseline

---

## Background (all code-level facts)

| # | Problem | Fact |
|---|---------|------|
| P1 | **Background scheduling trigger is broken** | After ADR-068 no path creates `Pending` consolidation nodes (`memory_store` writes Episode only; distillation promotion writes Active directly), while `ConsolidationBgTask::should_run()` counts **Pending nodes** (`get_pending_for_consolidation` scans `KNOWLEDGE`-labelled `status=Pending`). In a real deployment `run_episodic_distiller_step` therefore **never runs**. Unit tests and e2e only called `distiller.run()` directly, so M7 acceptance never covered the scheduler-to-distiller path. |
| P2 | **Config is read once, no hot reload** | The distiller switch and parameters are snapshotted from the manifest when the consolidation pipeline starts (`agent_core.rs` `start_consolidation_pipeline`); they cannot change at runtime, and the Desktop has no channel to write the manifest. |
| P3 | **The distillation model is hardcoded** | It is the id of the first model in `global_provider_list`; there is no model config item and it does not reuse the summary model (`default_compact_model`) mechanism. |
| P4 | **Distillation prompts cannot be overridden per agent** | `EXTRACTION_SYSTEM_PROMPT` and `JUDGE_SYSTEM_PROMPT` are hardcoded Grafeo constants. The ADR-068 revision moved the Grafeo trio (extraction / conflict-classification / generalization) out of the override allowlist, and the two distiller prompts were never configurable. |
| P5 | **No UI entry, and the legacy button is misnamed** | The memory panel has no distiller settings, and the "merge nodes" (consolidate) button only does episodic cleanup after ADR-068 — it neither merges, distills, nor promotes. |

## Decision

**D1 — decouple the trigger from legacy Pending.** Background distillation no longer
depends on counting `Pending` nodes. The provider trait gains
`count_unconsolidated_episodes()`, counting `EPISODIC` entries where `consolidated=false` and not
skipped by the distiller.

The independent trigger condition (all must hold):

```
interval elapsed (distiller_interval_minutes, default 60)
  AND (unconsolidated episode backlog >= distiller_accumulation_threshold (default 50)
       OR conversation idle >= distiller_idle_minutes (default 30))
```

Legacy offline consolidation keeps its Pending condition; the two pipelines trigger
independently and never block each other.

**D2 — manual distillation endpoint.**

- `POST /memory/distill` (Runtime localhost) runs `run_episodic_distiller_step` once with
  force semantics, bypassing the interval and sharing the implementation with the
  background task.
- `GET /memory/consolidation/status` additionally returns `distiller_enabled`, the effective
  config, and the last `DistillerResult` (promotion count and time).

**D3 — layered config, following the existing platform convention.**

- `manifest [memory.distiller]` = **package author default** (declarative in the `.agent`, with new
  model / interval / accumulation / idle fields).
- `{work_dir}/config/agent_config.json` = **runtime layer**: on first run, if
  `agent_config.json` has no distiller parameters, initialize once from the manifest
  defaults; after that the Desktop only reads and writes `agent_config.json`. All fields are
  `Option`, where `None` falls back to the manifest and then to the system default — the same
  shape as the existing `temperature` and `context_window` fields.

**D4 — field set**

`AgentConfig` (`agent_config.json`) gains:

| Field | Type | Default when the manifest omits it |
|-------|------|-----------------------------------|
| `distiller_enabled` | `Option<bool>` | false (keeps ADR-068 opt-in) |
| `distiller_model` | `Option<CompactModelRef{provider_id, model_id}>` | none (see D5) |
| `distiller_interval_minutes` | `Option<u64>` | 60 |
| `distiller_accumulation_threshold` | `Option<usize>` | 50 |
| `distiller_idle_minutes` | `Option<u64>` | 30 |

`ManifestDistillerConfig` gains the same initial-value fields (enabled already exists;
model uses separate `provider_id` / `model_id` to match `CompactModelRef`; the rest are
snake_case identical).

**D5 — model selection.** The UI reuses the summary model dropdown wholesale
(`GlobalCompactModelCard`: vault keys, `provider::model` options, optimistic update, rollback on
failure), but stores an **independent** `distiller_model` field and MUST NOT implicitly reuse
`default_compact_model` — distillation quality and cost differ from summarization, so the two
are allowed to differ.

Effective resolution chain: `agent_config.json` → manifest `[memory.distiller].model` →
`default_compact_model` (global fallback) → the first model in the provider list (the current
behaviour).

**D6 — distiller prompts join the ADR-063 allowlist (per agent).**

- `OVERRIDABLE_PROMPTS` gains `distiller-extraction.md` (Step 2a structured extraction) and
  `distiller-judge.md` (Step 4 judge arbitration).
- `AgentCore` gains two `Arc<RwLock<Option<String>>>` slots; `reload_prompts_into_core`
  refreshes them; `DefaultEpisodicDistiller::run` takes override parameters, keeping the Grafeo
  constants as the built-in default.
- The Debug panel PromptList enumerates from the server, so the new whitelist entries appear
  automatically with no frontend change.
- `conflict-classification.md` and `generalization.md` are **not** restored (their producers are
  gone).
- This partially reverses the ADR-068 revision entry that removed the Grafeo overrides, and
  only for these two: the original reason (the Grafeo distillation path does not go through the
  prompts overlay) no longer holds now that this ADR puts the distillation path on the per-agent
  config surface.

**D7 — retire the legacy "merge nodes" entry.** The manual entry becomes "distill now"
(D2) and the memory panel consolidate button (zh locale 合并节点) is retired. Episodic
cleanup continues to run automatically on the periodic cycle, with no manual entry; a
separate explicit "clean up" button can be added later if needed.

**D8 — runtime hot reload.** `RuntimeConfigOverrides` (RuntimeConfigUpdate) gains the D4
fields, passed through the whole chain from the Gateway `PUT /api/agents/{id}/config` to
`AgentCore.apply_runtime_config`.

**The final implementation differs from the first draft**: a distiller config change does
**not** rebuild the background task. `ConsolidationTimer` keeps its scheduling policy in an
internal `RwLock<SchedulerConfig>`; after `update_config()` swaps the value, the **background loop
re-reads it on every tick** (effective within ≤60s). The timer keeps its idle / backlog / last-run
state, so a config change neither spuriously triggers nor delays distillation. Only when the
pipeline has not started yet does the next `start_consolidation_pipeline` (agent start or manual
rebuild) use the new config. Prompt slots use the existing reload path; `AgentCore
.rebuild_consolidation_pipeline_if_running()` is kept as a compatibility entry name whose actual
behaviour is this `update_config` hot swap.

## Delivery path

| # | Work item | Content | Status |
|---|-----------|---------|--------|
| W1 | Trigger fix | provider `count_unconsolidated_episodes` + Grafeo implementation; `ConsolidationTimer` / `run_consolidation` decoupled trigger criteria; interval / backlog / idle checks | done `ecad9cd7` |
| W2 | Manual endpoint | `POST /memory/distill` + `consolidation/status` extension (sharing `run_episodic_distiller_step`); fixed the embedding closure `block_on` panic via a `block_in_place` bridge | done `5d8fc2e2` |
| W3 | Config chain | new `AgentConfig` + `ManifestDistillerConfig` fields; `RuntimeConfigOverrides` passthrough; `apply_runtime_config`; `ConsolidationTimer.config` → `RwLock` hot swap | done `abd32cbb` |
| W4 | Prompt override | allowlist +2 (`distiller-extraction.md` / `distiller-judge.md`); AgentCore slots; `DistillerConfig` override fields; `Distiller::run` consumes them; reload; Debug PROMPT_ENTRIES +2 | done `c8c8426b` |
| W5 | UI | memory panel "memory distillation" card (switch / model dropdown / interval / distill now / last run); consolidate button replaced, legacy action deleted | done `f4eaf46e` |
| W6 | Tests and docs | scheduler trigger tests (backlog / idle / manual); config hot-swap test; prompt override tests; ADR-071 status | done |

## Acceptance matrix

| Item | Method | Status |
|------|--------|--------|
| Periodic trigger | `should_run_distill` interval gate; `test_distiller_trigger_*` | pass |
| Idle trigger | idle >= threshold with insufficient backlog → an independent branch fires | pass |
| Manual trigger | `POST /memory/distill` → force run (still respects opt-in); 409 when disabled | pass |
| Off by default | no config → `SchedulerConfig::default().distiller_enabled=false` | pass |
| Config hot reload | PUT agent config → `RwLock` `update_config`, effective ≤60s; `test_distiller_trigger_live_config_update_d6` | pass |
| Model selection | `resolve_distiller_model_id` four-layer chain tests; UI dropdown → `CompactModelRef` | pass |
| Prompt override | Grafeo `test_d7_prompt_overrides_*` (reaches the LLM call site / None fallback); AgentCore projection test; visible in Debug PROMPT_ENTRIES | pass |
| Legacy retirement | UI consolidate → "distill now"; store `consolidate` action removed; episodic cleanup still periodic | pass |
| Regression | memory 30 / grafeo 290 / runtime lib 1378 (1 pre-existing failure `restart_after_compression_preserves_todo_state`) / desktop tsc + vitest 357/358 (1 pre-existing `formatTime` failure); clippy 0 new | pass |

## Relationship to other ADRs

| ADR | Relationship |
|-----|-------------|
| ADR-068 | **Completes / revises** — M4/M7 scheduler wiring (fixes P1); extends the config surface (P2–P5); partially reverses the prompt-override removal entry, for the two distiller prompts only (D6) |
| ADR-063 | **Extends** — `OVERRIDABLE_PROMPTS` +2 |
| ADR-053 | **Depends on the precedent** — `summary.md` override and Debug PromptList |
| ADR-062 | **No conflict** — quality gates such as keyword sanitize stay unchanged at the LLM boundary |

## Revision note (2026-10): the config surface narrowed to "project then merge"

The **trigger criteria, scheduler wiring, manual distillation entry, and model
selection all stay unchanged** — the production incident was not in the trigger, since logs
showed the interval, backlog, and idle conditions were satisfied every time. What changed
is inside the distiller, so two config surfaces contracted:

| Original clause here | What 2026-10 actually does |
|---------------------|-----------------------------|
| D6: `OVERRIDABLE_PROMPTS` +2 (`distiller-extraction.md` for Step 2a extraction, `distiller-judge.md` for Step 4 arbitration) | **Merged into a single slot `distiller-merge.md`.** The extraction and judgement stages are gone; the distiller now makes one LLM call that decides `merge` / `no_merge` / `contradicts` between a new statement and K candidates, so two slots had two consumers. The allowlist is now system + 5 entries. |
| The 5 promotion gates and tombstones left over from ADR-068 | **Converged into a single knob `min_importance`** (`[memory.distiller].min_importance`, default `0.0`, meaning project everything). Episodes below the value are not projected, but leave **no tombstone** and are reconsidered each round — tightening then loosening cannot lose history, which is what removing tombstones depends on. |
| Clustering and judgement parameters in `DistillerConfig` | Keep `batch_size` (100), `merge_candidate_k` (5), `merge_recall_threshold` (0.65); delete the clustering and judgement fields. |

**On `merge_recall_threshold` being low (0.65)** — a question that comes up often. The
threshold only decides *which old nodes get sent to the model to judge*; it does not decide
*whether* to merge. Below the threshold the episode is projected directly with zero calls; above
it, there is one extra `no_merge` adjudication costing a small JSON. So **the cost of a
false candidate is one call, while the cost of a missed candidate is a duplicate node that is never
reunited**. Loosening is the directional-safe choice. If the panel looks too dense, adjust
`min_importance` first, not this threshold — lowering the threshold only sends more episodes
around the model into the store directly, creating more duplicates.

**Operational note**: when candidates are recalled but no model is available, the episode stays
"deferred" rather than guessing. The production path always supplies a model
(`run_episodic_distiller_step` takes `&dyn ConsolidationLlm`), so this only happens on a
model misconfiguration, and appears as "the backlog number does not drop but the
consolidated count does not grow". When debugging, first confirm distillation has an
available model, then look at the trigger parameters.
