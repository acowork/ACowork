# ADR-062: Centralizing Memory Quality Parameters and the Retrieval Quality Gate (MemoryQualityConfig)

> **Chinese source of truth**: [ADR-062](../zh/ADR-062-memory-quality-config-and-retrieval-gate.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

> ✅ **M5 fully landed and green (Option A)** — auto_inject first-turn trigger (§6.1) + the keyword
> write-time quality gate (§6.2.1) + `keyword_index` write-time injection into `object` (§6.2 Plan Y /
> §6.5 steps 2a/2b).
>
> - Current code: `auto_inject_enabled` defaults to `false` (enabled per agent via
>   `[memory.quality].auto_inject_enabled = true` in the manifest; when on it triggers once per
>   session's first turn); `keyword::sanitize`'s quality gate is always-on (called from both the
>   memory_store LLM boundary and the instant persistence boundary);
>   `quality.keyword_index` defaults to `false` (explicitly enabled per agent in the manifest).
> - **Post-M5 correction**: M5 briefly changed the `auto_inject_enabled` default from `false` to
>   `true`; because it duplicated recall with the LLM's autonomous `memory_recall` path (both use a
>   user message as the query, so core nodes necessarily overlap — see
>   [05-memory.md §0](../../design/zh/05-memory.md#0-分层原则) "retrieval-injection row"), the default was
>   reverted to `false` (per-agent opt-in), and the `memory_recall` tool description now carries an
>   anti-duplicate-recall hint ("do NOT re-run the same query").
> - M5 benchmark (`memory_m5_bench.rs`): keyword hit@5 0.0000→1.0000, Precision@5 0.5000→0.8750,
>   Recall@5 0.6250→1.0000, MRR 0.6250→1.0000, no regression.
> - Commit history: `15654af0` (a safe snapshot including the M5 draft) → `b117f901` (reverting only
>   the M5 code delta) → `6de0caf2` (the §6.2.1 quality gate doc) → the M5 landing commit.
> - Rollback path: all switches are parameterized (`quality.auto_inject_enabled` /
>   `quality.keyword_index`), so rollback cost = change a config value.
>
> ✅ **Retrieval parameters wired up (post-M5 increment)** — after auditing which
> `memory_recall` parameters were inert, two were implemented:
> - **Time filtering (#1)**: `since` / `until` were previously only validated and never applied (no
>   code ever wrote `filters.time_range`). Now wired through three layers —
>   `MemoryProvider::get_node_created_at` (a new trait method; `GrafeoProvider` reads the node's
>   `created_at` property and `InMemoryProvider` implements it), a `MemoryManager::retrieve`
>   post-filter (mirroring `exclude_session_id`'s keep-on-unknown policy), and the `memory_recall`
>   tool writing `filters.time_range` (one-sided bound filling: `since` alone → `[since, now]`,
>   `until` alone → `[epoch, until]`).
> - **Configuration consistency (#6)**: `memory_recall` previously hard-coded
>   `MemoryManagerConfig::default()` (`memory_recall.rs:184`) and ignored the agent's manifest
>   quality config. `MemorySessionHandle` now holds the agent's `MemoryManagerConfig` and the tool
>   reads it from the handle — the same config as auto_inject (min_score / graph_expand / …), so both
>   paths behave identically.
> - Tests: all 20 `memory_recall` tool tests green (new: since/until e2e across both providers
>   — InMemoryProvider + GrafeoStore, the manager-level `time_range` filter, direct
>   `get_node_created_at` trait tests, spec description assertions); memory 30 / grafeo 291 /
>   runtime 1138 / memory_e2e 4 / p1p2 17 / m4 1 / m5 1 all green, clippy 0 warnings.
> - **Still to wire (later)**: the three `search_mode` strategies, `privacy_levels`, `session_id`
>   filtering, weighted RRF — most are constrained by ADR-062 P3 decisions (benchmark evidence
>   required first), see §9.

## Status

Implemented (2026-09)

## Date

2026-09

## Decision Makers

大鱼 (Dayu)

## Predecessors

- [ADR-051](../zh/ADR-051-runtime-memory-provider-decoupling.md) (Runtime ↔ Grafeo decoupling)
- [ADR-057](../zh/ADR-057-compaction-distillation-into-graph.md) (compaction distillation into the
  graph; triples deleted)
- [ADR-060](../zh/ADR-060-prompt-cache-friendly-context-block-reorg.md) (context block reordering;
  the retrieval injection position)
- [05-memory.md](../../design/zh/05-memory.md) (the memory system design doc — the P1/P2 data quality
  baseline)

---

## 1. Decision Summary

The P1/P2 memory data-quality changes (`bugfix/memory`, G1-G13/G20) have landed and are fully
green; **write quality is clearly improved and verifiable**, but **retrieval quality has only seen
local improvements, insufficient to justify re-opening `auto_inject` or letting `keywords`
participate in retrieval**. The core gaps:

1. **The retrieval path does not exclude `Dormant` nodes** — design §5.2 states "Dormant does not
   participate in regular retrieval", but no layer of the chain (`manager.rs` →
   `provider_impl.rs` → `grafeo.rs` → `grafeo-engine`) filters by `status`. Decay/cleanup only
   reduced storage volume and **did not reduce the junk in retrieval results** — which is exactly
   why `auto_inject` was closed.
2. **`hint_weights` (the RRF weights) do not participate in ranking** — `retrieval.rs` annotates
   `_text_weight` / `_vector_weight` as "Reserved", so all four hint-weight configurations are dead
   configuration.
3. **Retrieval quality parameters are scattered across 5+ files with no quantified benchmark** —
   untunable, incomparable, unrollbackable.
4. **The default values in the `memory_store` tool description anchor the LLM** — the
   confidence/importance distribution collapses, destroying the discriminative power of decay and
   the consolidation gate.

This ADR decides:

1. **P0: exclude Dormant nodes from the retrieval path** (`exclude_dormant` filter, on by default).
   This is the single line that makes decay/cleanup actually feed back into retrieval quality, and
   it is the precondition for re-opening `auto_inject`.
2. **P1: add `MemoryQualityConfig`** (memory layer, overridable by the `.agent` package) to
   centralize the quality parameters that are "effective but hardcoded", eliminating scattered
   literals.
3. **P2: a benchmark gate** — re-opening `auto_inject` and letting `keywords` participate in
   retrieval **must be predicated on the quantified metrics in `eval.rs` / `retrieval_metrics.rs`
   meeting their thresholds.** No unfounded toggles.
4. **P3: de-anchor the `memory_store` tool description** — the schema already landed ahead of time in
   a zero-numeric form (§6.2); this ADR confirms the current state and pushes M3.6 data calibration:
   guide the LLM toward discretized scores along evidence dimensions, and recalibrate the thresholds
   from distribution data (see §6.6).

**Non-goals** (not discussed here):
- The semantic quality of the memory content itself (LLM scoring accuracy) — this ADR defines only
  the capture/filter/tuning mechanisms.
- Replacing the embedding model and vector quality — a separate topic.
- The full weighted RRF implementation for `hint_weights` — only built if the P2 benchmark proves it
  necessary.

---

## 2. Background and Current-State Inventory (code-level facts)

### 2.1 P1/P2 change overview and quality conclusions

| Dimension | Conclusion | Evidence |
|-----------|-----------|----------|
| Write quality | ✅ **clearly improved** | typed `privacy`/`importance`/`source`/`keywords` fully persisted (`grafeo/consolidation/instant.rs:238,266`); dedup gates `0.95`/`0.90` (`instant.rs:52,58`); offline consolidation confidence gate `<0.3→Dormant / ≥0.7→Active` (`offline.rs:132-148`); the three episode cleanup rules (`offline.rs:392`) |
| Retrieval quality | ⚠️ **local improvements** | graph expand genuinely consumes edge weights + thresholds `[0.1,0.15,0.2]` (`spreading.rs:159-163`); Identity full-label retrieval (`manager.rs:319`); abstention prompt injection (`manager.rs:507`) |
| Retrieval quality gaps | 🔴 **not closed** | **Dormant not excluded**; **RRF weights dead**; **no benchmark numbers** |

### 2.2 Decisive fact: no Dormant filter in the retrieval chain

Design §5.2: "Dormant does not participate in regular retrieval but is retained" (**not deleted**).
The full-chain audit:

| Layer | File | status filter |
|-------|------|---------------|
| Retrieval orchestration | `acowork-memory/src/manager.rs` `retrieve` | ❌ session exclusion + dedup only (`manager.rs:380-545`) |
| Provider | `acowork-grafeo/src/provider_impl.rs` | ❌ |
| Native retrieval | `acowork-grafeo/src/grafeo.rs` `search_with_filter` | ❌ score filter only |
| Engine | `grafeo-engine search.rs` | ❌ the text/vector indexes contain all nodes |

**Implication**: G2 (decay→Dormant) and G13 (episode cleanup) currently only reduce storage volume;
Dormant nodes still appear in retrieval results. `auto_inject` was originally closed because of
"junk in context", and **that reason still holds**.

### 2.3 Decisive fact: `hint_weights` is dead configuration

`manager.rs:1024` defines four weight sets (Semantic 0.8/0.2, Identity 0.3/0.7, …), passes them to
`hybrid_search_full`, but `grafeo/retrieval.rs:239-240` states explicitly:

```rust
_text_weight: f32,   // Reserved for future weighted RRF implementation
_vector_weight: f32, // Reserved for future weighted RRF implementation
// Weight scaling after RRF is meaningless...
```

Ranking therefore relies only on **RRF rank + PageRank boost**; the design §6.6 "dynamic weights"
never took effect.

### 2.4 Decisive fact: the retrieval quality parameters are scattered

| Parameter | Current value | Location | Effective | Configurable |
|-----------|---------------|----------|-----------|--------------|
| RRF k | 60 | hardcoded in grafeo-engine | ✅ | ❌ |
| hint_weights (4 sets) | 0.8/0.2 etc. | `manager.rs:1024` | ❌ dead | ❌ |
| min_cosine (cosine domain, default) | 0.3 | `MemoryQualityConfig` | ✅ | ✅ existing |
| ~~min_score (RRF domain / auto_inject)~~ | ~~0.0 / 0.3~~ | deleted → see ADR-082 | — | — |
| graph expand thresholds | `[0.1,0.15,0.2]` | `spreading.rs:42` | ✅ | ✅ (builder) |
| min_edge_weight | 0.1 | `spreading.rs` | ✅ | ✅ (builder) |
| DECAY_PER_HOP | 0.7 | `spreading.rs:105` | ✅ | ❌ hardcoded |
| edge weight λ / cap | 0.01 / 0.8 | `semantic/graph.rs:12` | ✅ | ❌ hardcoded |
| decay parameters | 7 fields | `DecayConfig` (`memory/types.rs:606`) | ✅ | ✅ existing |
| dedup thresholds | 0.95 / 0.90 | `instant.rs:52,58` | ✅ | ❌ hardcoded |
| consolidation gates | instant 0.85 / offline 0.7+0.3 / generalization 0.8 | `instant.rs:68`, `offline.rs:132,138`, `generalization.rs:427,473` | ✅ | ❌ hardcoded (three sets) |
| PageRank weight | 0.1 | `MemoryManagerConfig` | ✅ | ✅ existing |
| **Dormant retrieval exclusion** | **none** | **missing** | — | — |

### 2.5 Decisive fact: the anchoring problem in the `memory_store` tool description

> **2026-09 review**: the "default-value anchoring schema" described here used to be the code
> reality; at review time the `confidence`/`importance` schema text in `memory_store.rs` had
> **already been changed to a zero-numeric form** (matching the §6.2 target), i.e. D4's schema change
> landed ahead of time. This section preserves the historical fact and the code-fallback inventory;
> D4 (§6) is repositioned as "confirm landed + M3.6 threshold calibration".

The historical anchoring text (what the LLM actually saw):

```jsonc
"confidence": { "description": "... High confidence (>=0.85) creates an Active node;
    lower creates Pending for later verification. Default 0.7 for knowledge/procedure,
    0.85 for autobiographical." }          // ← anchoring point 1
"importance":  { "description": "... Higher importance resists forgetting. Default 0.5." }
                                             // ← anchoring point 2
```

Code fallbacks (used when the LLM supplies nothing at all):
`memory_store.rs:20,26`: `DEFAULT_CONFIDENCE=0.7` / `AUTOBIO_DEFAULT_CONFIDENCE=0.85`;
`instant.rs:71`: `DEFAULT_CONFIDENCE=0.7`; `instant.rs:266`: `importance.unwrap_or(0.5)`.

**Why anchoring matters**: if the schema says "Default 0.7/0.5", an unconfident LLM simply adopts
the "safe default", collapsing the confidence/importance distribution onto a few values and
destroying its discriminative power; downstream decay (FLOOR/BOOST_CAP depend on importance) and the
consolidation gate (0.7/0.3 depends on confidence) then fail. The current schema has eliminated this
risk, but **whether the distribution is genuinely discretized and whether the thresholds need
rescaling still requires M3.6 data validation** (§6.6).

---

## 3. Decision D1 (P0): Exclude Dormant Nodes From the Retrieval Path

### 3.1 Decision

Add a `status != "Dormant"` filter at the merge stage of the retrieval chain, exposed as
`MemoryQualityConfig.exclude_dormant` (default `true`).

### 3.2 Implementation location (preferred)

At the `all_results.retain(...)` in `grafeo.rs search_with_filter`, or before `manager.rs` builds
`best_by_id` after merging. **Prefer the manager layer** (it works for all Provider
implementations, including test doubles), then the grafeo native layer as a fallback.

**Precondition (M1 step 1)**: the `MemoryProvider` trait currently has only `get_node_content` /
`get_node_session_id` (`acowork-memory/src/provider.rs:246,256`) and **no `get_node_status`**.
Filtering by status in the manager layer first requires adding
`fn get_node_status(&self, node_id: u64) -> Result<Option<NodeStatus>>` to the trait, implemented in
both `GrafeoProvider` (`provider_impl.rs`) and the test doubles — otherwise the filter has nowhere
to land.

**Relation to graph_expand**: the graph expansion seeds come from `all_results`
(`manager.rs:385-406`, which includes Dormant), while the Dormant filter runs before `best_by_id` is
built (`:408`). The resulting semantics are therefore: **Dormant nodes may still serve as graph
expansion seeds (preserving graph bridging) but never appear in the final retrieval results.**

### 3.3 Semantic details

- **Purged** nodes are physically deleted and need no handling.
- **Pending** nodes (low confidence, pending verification) are by design "retrievable but low
  confidence". By default they **participate in retrieval and are naturally down-weighted by
  confidence ordering**; they are not filtered separately (avoiding over-tightening recall).
- Whether a `Dormant` hit counts toward `access_count` / restores Active is a later behavioural
  question; this ADR defaults to **not restoring** (to avoid retrieval itself creating "fake
  activity") — see §9.

### 3.4 Acceptance

- New test: after `transition_to_dormant`, the retrieval result excludes the node; Active/Pending
  are still returned.
- Regression: the existing `memory_p1p2_e2e.rs` and `memory_e2e.rs` stay green.

---

## 4. Decision D2 (P1): Add `MemoryQualityConfig`

### 4.1 Decision

Add `MemoryQualityConfig` to the memory layer, gathering the quality parameters that are "effective
but hardcoded", with per-agent overrides (injected via the `.agent` package manifest, reusing
ADR-051's decoupling channel).

```rust
pub struct MemoryQualityConfig {
    // ── Retrieval ──
    pub exclude_dormant: bool,                 // default true (D1)
    pub min_score: f32,                        // RRF domain; auto_inject and the default both route here
    pub graph_expand: GraphExpandQuality {     // thresholds / min_edge_weight / decay_per_hop
        early_stop_thresholds: Vec<f32>,       // default [0.1, 0.15, 0.2]
        min_edge_weight: f32,                  // default 0.1
        decay_per_hop: f64,                    // default 0.7
    },
    pub edge_weight: EdgeWeightQuality {       // lambda / cap
        lambda: f64,                           // default 0.01
        cap: f32,                              // default 0.8
    },
    pub pagerank_weight: f64,                  // default 0.1 (merging the existing MemoryManagerConfig field)
    // ── Writing ──
    pub dedup: DedupQuality {
        knowledge_threshold: f32,              // default 0.95
        procedure_threshold: f32,              // default 0.90
    },
    pub consolidation: ConsolidationQuality {
        direct_active_threshold: f32,          // default 0.85 (instant.rs:68 instant extraction → Active)
        pending_upgrade_threshold: f32,        // default 0.7 (offline.rs:138 offline consolidation Pending→Active)
        dormant_confidence: f32,               // default 0.3 (offline.rs:132 offline consolidation → Dormant)
        min_pending_age_hours: u64,            // default 1 (offline.rs:640)
    },
    pub keyword_index: bool,                   // default false (enabled after the P2 gate passes)
}
```

### 4.2 Boundaries and trade-offs

- **Not migrated**: `DecayConfig` (already 7 centralized fields) and `MemoryManagerConfig`
  (retrieval budget / injection budget) stay independent to avoid a big bang.
- **Overlapping fields with `MemoryManagerConfig`**: `MemoryQualityConfig.min_score` /
  `pagerank_weight` are synonymous with `MemoryManagerConfig.default_min_score` / `pagerank_weight`
  (`manager.rs:167,170`). Rule: **`MemoryQualityConfig` is the new source; the old fields are marked
  deprecated** (read `MemoryQualityConfig` first at runtime, falling back to the old
  `MemoryManagerConfig` value when unset), with cleanup in a separate change. They never both take
  effect.
- **The consolidation thresholds are three sets, not one**: the code has three independent sets —
  instant extraction `≥0.85→Active` (`instant.rs:68`), offline consolidation `≥0.7→Active /
  <0.3→Dormant` (`offline.rs:132,138`), and experience generalization Pending→Active `≥0.8`
  (`generalization.rs:427,473`, the ProceduralNode path). D2's `ConsolidationQuality` therefore
  splits into `direct_active_threshold` / `pending_upgrade_threshold` / `dormant_confidence` to
  cover reality. **Whether the generalization 0.8 and the offline 0.7 should be unified is an open
  question** — different confidence lines per node type (a false positive on a procedural node
  costs more) is semantically defensible, so this ADR parameterizes them separately without forcing
  unification, leaving the decision to the post-M3.6 calibration.
- **Parameterizing RRF k** (P3) depends on grafeo-engine version support and is deferred.
- **`hint_weights`**: currently dead. If the P2 benchmark proves weighting is meaningful, implement
  it; otherwise **delete the dead code** (Rule of three / YAGNI).
- **Per-agent injection mechanism (M2 TODO)**: today `MemoryManagerConfig` is only constructed via
  `Default` (`agent_core.rs:810-812`) — **no manifest→config injection pipeline exists**. M2 must add
  a `.agent` manifest `[memory.quality]` section parser + Provider factory injection. ADR-051 only
  solved Runtime↔Provider decoupling and did not cover config injection.
- Every field's default must match the current code so that "zero config = current behaviour", which
  makes the landing smooth.

### 4.3 Acceptance

- All parameters are overridable via configuration and take effect; defaults match current
  behaviour (snapshot comparison test).
- `cargo test -p acowork-memory -p acowork-grafeo` all green.

---

## 5. Decision D3 (P2): The Benchmark Gate

### 5.1 Decision

Re-opening `auto_inject` and enabling `keyword_index` are **both predicated on the benchmark meeting
its thresholds**, never on "it feels like the quality improved".

### 5.2 Metrics and thresholds (first cut, calibratable)

Reusing the existing `grafeo::eval` (`eval_information_extraction` / `eval_abstraction`) and
`retrieval_metrics` (NRR / Precision@k / Recall@k):

| Metric | Threshold (proposed initial value) | Measurement |
|--------|-----------------------------------|-------------|
| Retrieval Precision@5 (after the Dormant exclusion) | ≥ 20% improvement over the current value **and** ≥ 0.5 | `retrieval_metrics` + a fixed query set |
| Dormant junk entering context | = 0 (guaranteed once D1 takes effect) | sampling of hit cases |
| confidence/importance distribution variance | significantly increased (anchoring removed) | statistics on `memory_store` write samples |
| auto_inject injection hit rate | ≥ 60% of the time the result set is non-empty | before/after comparison |

### 5.3 The gate process

1. After merging D1 + D2 + D4 (de-anchored prompts), run a benchmark round and record the "before"
   numbers.
2. Only once D1 is in effect **and** the benchmark meets the §5.2 thresholds do we open
   `auto_inject` (also fixing its `min_score=0.3` score-domain problem — see §6.4).
3. `keyword_index` follows the same rule: enabled only with supporting data.

### 5.4 Acceptance

- Produce a benchmark report (a before/after comparison table) archived as the evidence for opening
  the switches.

---

## 6. Decision D4 (P3): De-anchoring the `memory_store` Tool Description

### 6.1 Decision

**2026-09 review status: the de-anchored schema already landed** — the `confidence`/`importance`
descriptions at `memory_store.rs:102-114` are already zero-numeric and purely
evidence-guided (matching the §6.2 target form). This decision is therefore repositioned from
"pending" to:

1. **Confirm the current state matches the §6.2 target form** (as the acceptance evidence, see §6.5)
2. **Keep the code fallback constants** (`DEFAULT_CONFIDENCE=0.7` /
   `AUTOBIO_DEFAULT_CONFIDENCE=0.85` / `importance.unwrap_or(0.5)`) as the defence for "the LLM
   supplied nothing at all" — this is not anchoring
3. **M3.6 threshold calibration** (§6.6) still has to run — the schema is de-anchored, but whether
   the distribution is discretized and whether the backend thresholds need rescaling must be
   validated with real write data.

### 6.2 Prompt rewrite (target form, zero-numeric)

**Key principle: the LLM does not need to know the backend's decision thresholds (0.85/0.3/0.7)** —
the Active/Pending/Dormant decision is a backend rule in `instant.rs` / `offline.rs`, and writing
thresholds into the prompt only creates two kinds of anchoring: default-value anchoring (lazily
adopting the "safe default") and gain-seeking anchoring (knowing that 0.85 triggers "immediately
Active" and systematically inflating scores). Hence the prompt is **zero-numeric and purely
evidence-guided**:

```jsonc
"confidence": {
    "type": "number",
    "description": "Your confidence in this knowledge (0.0-1.0), reflecting how certain
        you actually are. Anchor on evidence, not on a target value: base it on whether
        the statement is direct, explicit, recent, and from the user personally (higher),
        versus inferred, stale, or speculative (lower). Most routine observations are
        moderately certain — score them accordingly. Reserve very high scores for facts
        you would bet on; use very low scores for uncertain or contradicting signals.
        Do not inflate scores to make a memory seem more certain than it is."
},
"importance": {
    "type": "number",
    "description": "How critical is this memory to long-term value (0.0-1.0)?
        Higher resists forgetting. Distinguish core identity facts (near 1.0) from
        transient preferences (~0.3-0.5) from trivia (~0.1)."
}
```

### 6.3 Key points

> All the following are the current implemented form (2026-09 review); this section is the formal
> record of the design intent.

- **De-anchoring ≠ no guidance**: do not supply a "default value", and do not leak the backend
  decision thresholds (0.85/0.3/0.7) to the LLM — those are the rules of `instant.rs`/`offline.rs`;
  the LLM only needs to emit a continuous certainty score and the backend decides. Guide the
  reasoning along **evidence dimensions** (direct/explicit/recent/personal vs
  inferred/stale/speculative), which avoids both default-value anchoring and gain-seeking anchoring
  ("scoring 0.85 gets you immediately Active").
- **Trust the LLM's conservatism**: under a zero-numeric schema the LLM is naturally conservative
  and will rarely produce ≥0.85; **being able to produce 0.85 is itself a signal of "certain, not
  junk" and should be trusted.** Therefore **no pessimistic default hedging** (no temporary tightening
  of the immediate-effect line); high-score signals are taken at face value.
- **Keep the code fallbacks**: `DEFAULT_CONFIDENCE=0.7` / `importance.unwrap_or(0.5)` defend against
  "the LLM filled nothing in" and are not prompt anchoring — the two do not conflict.
- `test_memory_store_default_confidence` (`memory_store.rs:586`) and the autobio 0.85 assertion
  (`:915-918`) are **unaffected** — they verify the code fallback path, not the schema text.
- **Threshold reasonableness is decided by data**: de-anchoring shifts the absolute score scale, so
  the mapping between the code's `≥0.85→Active` / `<0.3→Dormant` / `≥0.7→Active` thresholds and the
  LLM's scores changes accordingly. This is not guessed; it is recalibrated from distribution data by
  the M3.6 calibration in §6.6.

### 6.4 Incidental fix: `auto_inject`'s `min_score` (⚠️ superseded by [ADR-082](./ADR-082-memory-storage-sqlite-vector-fts.md))

`memory/types.rs:132` sets `auto_inject`'s `min_score: Some(0.3)`, which sits in the RRF score domain
(`1/(k+rank)`, k=60 → a maximum of roughly 0.016) and would filter out nearly everything. After D2
lands it routes through `MemoryQualityConfig.min_score` (default 0.0).

> **ADR-082 correction**: the above argument only holds under **dual-source RRF**, and it missed the
> real hazard — `min_score = 0.0` on the **single-source vector path** (`score = cos − 1`, never
> positive) is equivalent to requiring `cos >= 1`, silently filtering out **all** vector results
> (the root cause of a Chinese-query `memory_recall` returning 0 rows). That "fused-score threshold"
> mechanism and the `min_score` field have been deleted entirely in favour of **each source applying
> its own gate in its own score domain before the rank fusion**, with the threshold renamed to
> `min_cosine` (absolute cosine domain, default 0.3). The full argument lives in ADR-082.

### 6.5 Acceptance

- The schema description contains no "Default 0.7/0.5/0.85" wording and **no backend decision
  threshold values** (0.85/0.3/0.7 trigger lines).
- For the same batch of memory writes, the **standard deviation of confidence/importance is
  significantly greater** than before (quantitative evidence that anchoring is gone).
- The full test suite is green.

### 6.6 Threshold calibration (M3.6)

De-anchoring makes scores discretized and discriminative but also shifts the absolute scale.
Threshold reasonableness must therefore be decided by data, not by guesswork:

1. After de-anchoring lands, first collect the real confidence/importance distribution (reusing the
   §5.2 "distribution variance" metric as calibration input).
2. Use the distribution data to **recalibrate** the `direct_active_threshold` /
   `pending_upgrade_threshold` / `dormant_confidence` / `dedup` threshold defaults in
   `MemoryQualityConfig` (D2 already parameterized them, so calibration = change a config, fully
   rollbackable).
3. The threshold semantics stay the same ("above 0.85 = worth activating immediately"); only the
   numbers move with calibration.
4. **No distribution-relative quantiles** (e.g. "only the top 25% of this batch becomes Active"):
   Active should reflect absolute credibility, and relativism distorts the semantics — if a
   conversation is entirely low-certainty, its highest score of 0.4 should not be forcibly judged
   Active.

---

## 7. Landing Priority and Milestones

| Milestone | Content | Depends on |
|-----------|---------|------------|
| M1 (P0) | Dormant retrieval filter + tests | — |
| M2 (P1) | `MemoryQualityConfig` landing + parameter consolidation + manifest `[memory.quality]` injection + the auto_inject min_score fix | M1 |
| M3 (P3) | `memory_store` prompt de-anchoring — **the schema already landed**, so this milestone narrows to "acceptance confirmation (§6.5) + write-path distribution instrumentation" | M2 |
| M3.6 (P3) | collect the confidence/importance distribution (requires lightweight write-path instrumentation, absent today) and recalibrate the threshold defaults (§6.6) | M3 |
| M4 (P2) | run the before/after benchmark and produce the report | M1–M3.6 |
| M5 (P2 gate) | ✅ **landed**: the auto_inject first-turn trigger mechanism + the keyword quality gate + `keyword_index` write-time injection into `object` (per-agent opt-in); the benchmark passed. **Follow-up correction**: the auto_inject default reverted `true → false` (per-agent opt-in, because it duplicated recall with `memory_recall`), see the status line | M4 |

Every milestone is independently mergeable and rollbackable (parameterization makes the rollback cost
= change a config value). Items still open after M5 landed are in §9 and in the benchmark report
§6.7 (P1: the PageRank non-determinism fix, vector index population verification; P3: weighted RRF,
Block C injection, etc.).

---

## 8. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| Excluding Dormant lowers recall (valuable dormant nodes vanish) | `exclude_dormant` defaults to true but is configurable; the benchmark compares the switch on/off |
| After de-anchoring the LLM systematically reports low confidence → memories go Pending/Dormant too early | **Trust the LLM's conservatism**: under a zero-numeric schema, a high score is itself evidence of certainty, so no pessimistic hedging (§6.3); if the measured distribution skews systematically low, M3.6 recalibrates the thresholds from data rather than re-inserting numbers into the prompt |
| Parameterization causes configuration explosion | only gather parameters that are "effective but hardcoded"; defaults = current behaviour; already-centralized config is not migrated |
| Weighted RRF unimplemented limits the tuning space | explicitly kept as P3; implement it only if the benchmark shows rank fusion is the bottleneck |

## 9. Open Questions

1. **Should a Dormant hit count as an access and auto-restore Active?** Design §5.2 says "referenced
   → restored", but having retrieval itself trigger a restore creates "fake activity". Lean: retrieval
   does not restore; only explicit user/conversation reference does.
2. **Does RRF k need parameterizing?** Depends on a grafeo-engine upgrade; recorded as P3.
3. **Is a weighted RRF for `hint_weights` worth implementing?** To be decided by the M4 benchmark;
   the currently dead code should be deleted or marked deprecated.
4. **How does `keyword_index` integrate**: injected into `content` for BM25, or a separate
   `metadata["keywords"]` index? To be refined once the benchmark shows a positive signal.

## 10. References

- [05-memory.md](../../design/zh/05-memory.md) (§5.2 Dormant semantics, §6.5 Abstention, §6.6
  retrieval weights)
- [ADR-051](../zh/ADR-051-runtime-memory-provider-decoupling.md)
- [ADR-082](./ADR-082-memory-storage-sqlite-vector-fts.md) (**supersedes §6.4 and the `min_score`
  decisions in §2.4**: each source applies its own gate + rank fusion)
- [review reports](../) (the source of the gap analysis)
