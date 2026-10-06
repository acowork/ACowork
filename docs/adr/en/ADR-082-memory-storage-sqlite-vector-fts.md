# ADR-082: Migrating the Memory Storage Backend to SQLite (vectors + FTS, no graph)

> **Chinese source of truth**: [ADR-082](../zh/ADR-082-memory-storage-sqlite-vector-fts.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Implemented

## Date

2026-09

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-051](./ADR-051-runtime-memory-provider-decoupling.md) — the direct prerequisite:
  the engine is touched by exactly one crate, `acowork-grafeo`
- [ADR-062](./ADR-062-memory-quality-config-and-retrieval-gate.md) — `MemoryQualityConfig`;
  its §6.4 `min_score` decision is finally superseded by D5 below
- [ADR-081](./ADR-081-global-search.md) — global search / conversation index
- [05-memory.md](../../design/en/05-memory.md)

---

## 1. Background

**1.1 The trigger — a 7–9 second startup.** An SSE agent (424 memories /
15,581 conversation messages) took 7–9s to start. Four measured startups:

| Phase | Cost | Note |
|-------|------|------|
| Phase A (package load / HTTP / MQTT) | ~0.6s | normal |
| `find_latest_session` (264 meta files) | 50–75ms (spikes to 2.9s) | cold-cache jitter |
| Memory store open + vector restore (424 rows) | ~0.6s | the ADR-081 P1-2 no-rebuild optimization working |
| **`ConversationIndex::open`** | **4735–5019ms, stable across all four** | **the main bottleneck** |
| SessionManager + first session | ~0.3s | normal |

**1.2 Root cause — a full 40MB WAL replay** (confirmed by reading the
grafeo-engine 0.5.42 source). The conversation index container is 46MB plus a 40MB WAL
(produced by bulk-indexing 15,581 messages, and never shrinking afterwards). Every open:

1. the periodic checkpoint (`checkpoint_timer.rs` → `try_checkpoint`) only calls
   `flush::flush` to flush the container; it **never writes WAL `checkpoint.meta` and never
   rotates the log** (`flush.rs` only calls `wal.sync()`);
2. with no metadata, recovery reads the WAL from seq 0 (`wal/recovery.rs` skips only
   files whose `sequence < cp.log_sequence`, and a single unrotated file has seq=0, so the
   condition never holds);
3. `apply_wal_records` (`database/mod.rs:1080`) has **no epoch filter**, so 15,581 node
   creations plus 2KB vector property writes replay verbatim — data already present in the
   container snapshot, purely redundant;
4. an explicit `wal_checkpoint()` does not help even with metadata: while a single file is
   unrotated `log_sequence=0`, and the skip condition `0 < 0` is false; `max_log_size`
   defaults to 64MB and is not exposed in the engine Config, so the 40MB log never rotates.

**1.3 Structural judgement — a patch cannot finish the job.** The engine is an
"load everything into memory on open" architecture, so open cost = full container load +
**BM25 rebuilt every time** (the engine does not persist postings, so `init_schema` /
`conversation_index.rs` rebuild on every open: ~0.3s for 15k messages, 2–4s at 100k) + a
vector restore scan (O(N) full read of vector properties). A WAL patch removes only one
term; the rest still grow linearly with data. The memory store has the same disease:
424 nodes correspond to a 9.2MB WAL (~22KB per node, from change history), so at
2000–4000 nodes open would rise to 3–6s.

**1.4 Mixed score domains — a design flaw in the engine fusion API.** `memory_recall`
once returned **zero results** for a Chinese query on a real agent. The engine
`hybrid_search` puts three different units into one `score` field
(`grafeo-engine-0.5.42/src/database/search.rs:385` plus
`grafeo-core-0.5.42/src/index/text/fusion.rs:58`):

| Hit path | What `score` actually means | Range |
|----------|------------------------------|-------|
| text + vector both | RRF rank score `Σ 1/(60+rank)` | ≈ `[0.016, 0.033]`, always positive |
| vector only | raw `-distance = cos − 1` | `[-2, 0]`, never positive |
| text only | BM25 score (with IDF) | drifts with the corpus |

Gating a fused score on `score >= min_score(0.0)` is, on the vector-only path, equivalent to
requiring `cos >= 1`. A Chinese query gets 0 BM25 hits, so it must take the pure
vector path, and is therefore always filtered out. Measured on a copy of the real store:
with `min_cosine=0.3`, the Chinese query returns 3 hits (cos 0.9999 / 0.838 / 0.819). This
is the third independent reason to leave the engine: **fusion and gating must be
implemented in-house for the score domain to be structurally guaranteed** (see D5).

**1.5 The graph layer is dead code (empirically).** A real store snapshot has
**edges = 0** (`acowork-grafeo/examples/count_edges.rs`); production code has no edge
creation path at all (`create_memory_edge` is only called from tests, `store_episode_with_session`
(HAS_MEMORY) has no production caller, and distiller / consolidation / triple_extraction build no
edges). Yet `enable_graph_expand` defaults to `true`, so **every recall runs a
spreading-activation BFS over an empty graph** and necessarily returns nothing.
spreading.rs, the edge weight formula, and graph-expansion dedup all serve a graph that does
not exist.

**1.6 Workload premise (the origin of the design constraints).** One store per agent,
**zero concurrency**, write frequency of minutes for the memory store and tens of seconds for
the conversation store. Real-time writes are not under pressure. This scenario does not
need a WAL-buffered architecture designed for high throughput; it needs "durable on
write, read-only on open".

## 2. Decision

**D1 — the storage backend becomes SQLite (rusqlite, bundled).** Drop the four
dependencies grafeo-engine / grafeo-storage / grafeo-core / grafeo-common for one rusqlite.
The `GrafeoStore` API boundary is kept (the payoff from ADR-051), so the 12 Runtime
call sites keep importing `GrafeoStore` plus their own types unchanged.

```sql
CREATE TABLE nodes(
  id         INTEGER PRIMARY KEY,
  label      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'Active',
  props      JSON NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_nodes_label ON nodes(label);

CREATE TABLE vectors(
  node_id   INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
  dim       INTEGER NOT NULL,
  embedding BLOB NOT NULL              -- f32 LE × dim
);

-- one virtual table per search field (Knowledge / Procedural / Autobiographical likewise)
CREATE VIRTUAL TABLE fts_episodic USING fts5(
  content, node_id UNINDEXED, tokenize = 'trigram'
);
```

**D2 — vector search is an in-memory cache plus brute-force scan (exact), dropping
HNSW.**

- Vectors are lazily loaded per label into an in-memory cache (currently
  15,581 × 2KB = 32MB, a sequential read of 3–10ms, one-off). Queries are pure in-memory
  SIMD cosine, sub-millisecond, with **recall = 100%** (HNSW is approximate, typically
  95–99% recall@10).
- HNSW is the root of this complexity: index persistence, restore, sync, and WAL
  replay all exist because "rebuilding an in-memory graph structure is expensive". A
  brute-force scan has no index to maintain — a vector is just a BLOB row in the table.
- **Ceiling** (from `ponytail:`): 100k rows ≈ 200MB resident and a few ms per query, imperceptible;
  1M rows ≈ 2GB is the signal to act. The upgrade path is sealed behind the single function
  `retrieval.rs::vector_search`: f16 storage (halving memory) → mmap vector file →
  in-process ANN (usearch and friends, where the index degrades into a discardable cache —
  rebuild on corruption, worst case falling back to brute force). The vector BLOB layout
  is common across all stages.

**D3 — text search uses FTS5 (trigram tokenizer) with a persistent index.**

- FTS5 maintains its index incrementally and durably, so **the entire class of
  "rebuild BM25 on every open" problems disappears**.
- Tokenization is `trigram` (SQLite ≥3.34): unicode61 would merge a run of Chinese characters
  into one long token — the same root cause as the Chinese BM25 failure in §1.4. Trigram
  supports Chinese substring matching (queries under 3 characters fall back to LIKE).
- `score = -bm25()`: FTS5 `bm25()` returns smaller values for better matches, so negating it
  makes "larger is better" and aligns the domain with vectors.

**D4 — the graph layer is deleted entirely.** Remove spreading.rs, the three
`graph_expand` call sites, edge weight computation (the edge-weight part of ADR-062),
`edge_types` / `store_edge` / the Session-HAS_MEMORY leftovers, and the edge queries in
GQL. **No edges table, no simulated graph traversal.** If memory association is genuinely
  needed later, adding an edges table plus application-level BFS is half a day of work — build it
  against a real requirement (YAGNI), and the data model can take it at any time.

**D5 — score domains and gating (each source applies its own gate, then rank fusion; the
decision is kept but the implementation is rewritten).** The original "hybrid retrieval
score domain and gating" decision is folded in here. With the engine retired the fusion code is ours, so the mixed-domain bug of §1.4 is structurally gone while the gating semantics stay:

- **Each source applies its own gate, and the results are unioned**:
  `kept = (vector hit AND cos >= min_cosine) ∪ (text hit)`. A candidate hit by the
  text source **must be kept even when its embedding is far away** — a lexical hit is
  independent evidence, otherwise weak or degraded embeddings would silently kill BM25
  hits along with them.
- **The threshold is defined in the absolute cosine domain**: `MemoryQualityConfig.min_cosine`,
  default `0.3`, overridable through manifest `[memory.quality] min_cosine`. With
  normalization `(1 + cos)/2 ∈ [0,1]`. The old `min_score` (fused domain) is not revived.
- **No fixed threshold for the text source**: BM25 includes IDF and drifts with the corpus, so
  there is no stable absolute domain.
- Fusion is equal-weight RRF (k=60); MMR reranking is implemented on pairwise
  similarity within the vector cache.
- The existing `min_cosine` implementations (manifest / provider_impl / abstention) migrate to
  the new fusion layer, and **the existing gating tests serve as the acceptance criteria**.

**D6 — durable on write, read-only on open.** SQLite manages WAL automatically
(batch durability by default, so a crash loses at most 100ms) and handles checkpointing
itself. **No application-level post-write checkpoint hook is needed** — the essential
difference from the grafeo approach: SQLite open is an O(1) lazy page load independent of store
size, the FTS5 index is persistent, and the vector cache can be preloaded on a
background thread. Startup time is permanently constant.

## 3. Rejected alternatives

| Option | Why rejected |
|--------|--------------|
| Patch grafeo-engine (rotate-before-checkpoint, 2 lines) | fixes only the WAL replay; BM25 rebuild and full container load still grow linearly with data; third-party engine semantics risk persists (this incident investigation was already expensive) |
| Application-side `wal_checkpoint()` after writes | ineffective while a single file is unrotated, and rotation is only triggered by an unexposed 64MB threshold — unreliable |
| Disable WAL for the conversation index | saves only the rebuildable conversation store; the memory store is primary data where the WAL *is* the durability guarantee |
| sqlitegraph (oldnordic) | **GPL-3.0-only is an outright veto** (static linking is contagious); 94 versions in 7 months spanning 3 major versions; no FTS; fundamentally "a younger grafeo", carrying the index-persistence coupling back unchanged |
| sqlite-vec / libsql | brute-force scan is already the exact answer at current scale, so ANN is not needed; kept as candidates on the D2 upgrade path |

## 4. Migration plan (three steps, each independently verifiable)

1. **Dual backend** — the SQLite backend implements the equivalent `GrafeoStore` API and the
  existing 188 tests pass against both backends.
2. **Migration validation** — real SSE data is migrated in (memory nodes via the export
  path; the conversation index rebuilt from JSONL, whose watermark mechanism already
  exists), comparing retrieval quality and startup time.
3. **Switch and delete** — the Runtime switches over and the grafeo-engine dependency,
  `index_persist`, spreading / the graph layer, and `migrate_legacy_store` are removed
  (roughly 2k net lines deleted).

**Execution status (implemented)**

- **Steps 1 and 2 are done**: the SQLite backend (memory + session meta + conversation index)
  landed and passes; the `ACOWORK_MEMORY_BACKEND` canary switch was removed in step 3.
- **Step 3 (switch + delete) is done**: `init_memory_provider` now goes to SQLite unconditionally;
  the `acowork-grafeo` crate (45 files / ~17k lines), the grafeo-engine /
  grafeo-common / grafeo-core dependencies, the `grafeo-backend` feature (formerly the
  default), 19 feature gates, `init_grafeo_backend`, and two `From<GrafeoError>` impls are
  all removed. D4 landed with it: `graph_expand` / `graph_expand_seeded` /
  `create_memory_edge` / `apply_pagerank_boost` / `enable_graph_expand` / `pagerank_weight` /
  `edge_types` and the manifest-side config are all gone (on the SQLite side these were
  already stubs returning empty, so behaviour is unchanged).
- **Two modules unrelated to storage moved out of grafeo**: the ADR-068 episodic distiller
  (`acowork-memory/src/consolidation/distiller.rs`, changing only the error type
  `GrafeoError` → `AcoworkError`) and retrieval quality evaluation
  (`acowork-memory/src/retrieval_metrics.rs`).
- **Three stores unified**: memory nodes, session meta (formerly `conversations/meta/*.json`), and
  the conversation index share `memory/private.sqlite` with one connection and one write lock;
  `acowork-core::workspace` is the single source of the private store path, the node
  clones the whole `private.sqlite` (including `-wal` / `-shm`), and the packaging exclusion
  rules were updated accordingly.
- **All legacy data sources and one-off import code are deleted** (a development-phase
  decision: no intermediate state is kept): the three import paths from `private.grafeo` →
  memory, `conversations/meta/*.json` → sessions, and `conversation_index.grafeo` / the standalone
  `conversation_index.sqlite` → the shared store, plus the `ConversationIndex::open` fallback to the
  standalone store. The old files (`meta/*.json`, `private.grafeo`,
  `conversation_index.grafeo`, the standalone `conversation_index.sqlite`) are safe to delete.
- **Kept**: `SCHEMA_VERSION` / `MIGRATIONS` / `apply_migrations` in `acowork-sqlite::schema` (gated on
  `PRAGMA user_version`). This is the SQLite library own version-upgrade mechanism, unrelated
  to the old data sources, and a long-term mechanism worth keeping.
- **Commits**: `61bb765e` / `69180f27` (delete one-off imports), `14c523a4` (delete
  `acowork-grafeo` and migrate distiller / retrieval_metrics), `5780a826` (delete the graph
  API, D4).

## 5. Consequences

**Upside**

- Startup drops from ~7–9s to **under 1s and stays O(1) permanently**, never degrading with data size.
- The Chinese BM25 failure (§1.4) is solved along the way by trigram tokenization.
- Vector search goes from approximate to exact.
- Four dependencies out, one in; consolidation batches gain real transactional atomicity.
- Backup becomes `VACUUM INTO` or a file copy; diagnosis uses standard SQLite tooling.

**Downside / risk**

- A 4–6 day migration window plus a one-off data migration.
- Vectors stay resident in memory (100k ≈ 200MB f32; f16 halves it, see the D2 ceiling).
- Losing future grafeo features (nothing currently planned depends on them).
- FTS5 trigram needs a LIKE fallback for queries under 3 characters (an implementation note).

## 6. Open questions

1. Should `props` be a JSON column or split into per-label wide tables? Start with JSON
   (flexible, fast enough) and project to columns only if hot-path property filtering
   appears.
2. When to preload the vector cache on a background thread (immediately after open vs before
   the first query) — this trades first-query latency against startup CPU contention;
   measure during migration.
3. Keep the conversation index watermark mechanism as is, or switch to a SQLite
   autoincrement cursor? Leaning toward the latter (simpler); decide during migration.
4. Should the `min_cosine` default vary by embedding provider (dimension / model)? A single
   default behaves differently across providers; needs evaluation-set data.
5. Introduce a reranker (a second ONNX model)? Memory and startup cost versus accuracy
   gains — decide once the evaluation set has data.
