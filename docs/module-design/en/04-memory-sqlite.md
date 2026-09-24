# acowork-memory + acowork-sqlite — Agent Private Memory Engine

**Position**: Storage + retrieval + offline distillation of Agent-private Memory, described as two crates.

- `acowork-memory`: Memory traits / types / manager / offline distiller / retrieval quality metrics — **storage-agnostic**.
- `acowork-sqlite`: Sole production storage backend; single-file SQLite (`memory/private.sqlite`), implementing `acowork_memory::MemoryProvider` as `SqliteStore`.

Runtime holds `Arc<dyn MemoryProvider>` (ADR-051) and never touches a concrete storage engine.

---

## 1. Goals & Boundaries

| Decision                    | Goal / Boundary                                                                |
| -------------------------- | ------------------------------------------------------------------------------ |
| Storage engine             | Single-file SQLite (rusqlite direct, no ORM, no Diesel). PRAGMA WAL + NORMAL + foreign_keys |
| Vector retrieval           | Same-database `vectors` table + application-layer cosine scan (ADR-082 C1)       |
| Full-text retrieval        | One FTS5 virtual table per Label, `tokenize='trigram'` (CJK substring match)   |
| Knowledge graph            | Application-layer `edges` table; graph traversal = SQL JOIN + early-stop       |
| Embedding                  | From Runtime `EmbeddingProvider` trait (Ollama / Remote fallback); Store never holds it |
| Lifecycle                  | Startup O(1) (no WAL replay, no BM25 rebuild) — ADR-082 §3                       |
| Schema evolution           | `PRAGMA user_version` + idempotent migration steps — ADR-082 D2                |
| Isolation                  | Per workspace/agent single file via in-process `MemoryProvider` — ADR-009       |

---

## 2. Crate Structure

### 2.1 `core/acowork-memory/`

```
crates/acowork-memory/
├── Cargo.toml
└── src/
    ├── lib.rs                  # Pub re-exports of trait/types/managers
    ├── provider.rs             # MemoryProvider trait (35+ methods)
    ├── store.rs                # MemoryStore trait (legacy, 16 methods)
    ├── manager.rs              # MemoryManager — Retrieve / Inject / Record three phases
    ├── quality.rs              # MemoryQualityConfig + DedupQuality + ConsolidationQuality
    ├── admin.rs                # MemoryAdminService — node CRUD / list / rebuild
    ├── keyword.rs              # Keyword sanitize / length gates (ADR-062)
    ├── judge.rs                # LLM Judge sampling decision (ADR-068 §5)
    ├── session_meta.rs         # SessionMeta + SessionMetaStore trait + TodoItem
    ├── types.rs                # Episode / KnowledgeNode / ProceduralNode
    │                           # AutobiographicalNode / MemoryQuery / SearchResult
    │                           # MemoryContext / DecayConfig / DecayScanResult ...
    ├── consolidation/
    │   ├── mod.rs              # EpisodicDistiller trait + SchedulerConfig
    │   └── distiller.rs        # DefaultEpisodicDistiller (server-side LLM distillation)
    └── retrieval_metrics.rs    # Retrieval quality metrics (Abstention / Conflict / Dedup)
```

**One-liner per module**:

- `provider.rs` / `store.rs`: define traits (implementation lives in `acowork-sqlite`).
- `manager.rs`: intermediate layer between Runtime and Store; orchestrates the three-phase lifecycle.
- `consolidation/distiller.rs`: 6-step offline distillation pipeline (ADR-068 §3.4), reads/writes via the `MemoryProvider` interface.
- `retrieval_metrics.rs`: retrieval quality metrics (see §6).
- `quality.rs` / `keyword.rs` / `judge.rs`: distillation-side gating config.

### 2.2 `core/acowork-sqlite/`

```
crates/acowork-sqlite/
├── Cargo.toml
└── src/
    ├── lib.rs                  # SqliteStore struct + error type + top-level factories
    ├── schema.rs               # SCHEMA_SQL + SCHEMA_VERSION + MIGRATIONS + apply_migrations
    ├── provider.rs             # MemoryProvider / MemoryStore trait implementations (~800 LoC core)
    ├── retrieval.rs            # hybrid_search / vector_search / text_search / RRF_K
    ├── admin.rs                # MemoryAdminService implementation (node browse / list / rebuild)
    ├── conversation.rs         # ConversationStore (ADR-082 §4 step 2, reusing nodes/vectors/FTS)
    ├── session_meta.rs         # SqliteSessionMetaStore (ADR-082 §4 step 3, sessions table)
    └── tests.rs                # Trait round-trip assertions (every field of every node type)

tests/
├── memory_chains_e2e.rs        # write/read / distiller / dedup end-to-end
├── shared_store_e2e.rs         # multiple instances sharing one DB (label boundaries)
├── session_meta.rs             # sessions table CRUD
└── store_isolation.rs          # same DB different label isolation (privacy boundary)
```

---

## 3. SQLite Schema (ADR-082 D1)

A single database with 6 physical tables and 6 virtual tables. All DDL in [`schema.rs::SCHEMA_SQL`] is idempotent; re-running on every `open` does not corrupt data.

### 3.1 Physical tables

| Table          | Role                                                                                |
| -------------- | ----------------------------------------------------------------------------------- |
| `nodes`        | One row per memory node; `label` distinguishes memory type; `props` JSON holds other fields |
| `vectors`      | Node embedding (f32 BLOB), FK `node_id` → `nodes.id` ON DELETE CASCADE              |
| `meta`         | Library-level metadata (currently only `embedding_dim`); reopen reads from DB to avoid stale caller dimension |
| `edges`        | Relations among consolidated-layer nodes (used by application-layer graph traversal), `(src_id, dst_id, kind, weight, props)` |
| `purge_log`    | Forgetting decay terminal table (archive first, then delete; field-encoded reason like `decay/purge/expiry`) |
| `sessions`     | Session metadata (ADR-082 §4 step 3, replacing `conversations/meta/*.json` sidecar)  |

### 3.2 Virtual tables (FTS5, trigram tokenizer)

| Virtual table           | Purpose                                              |
| ----------------------- | ---------------------------------------------------- |
| `fts_episodic`          | Episodic-layer `content` full-text index (CJK substring match) |
| `fts_knowledge`         | Consolidated Knowledge `content` full-text index      |
| `fts_procedural`        | Consolidated Procedural `content` full-text index     |
| `fts_autobiographical`  | Consolidated Autobiographical `content` full-text index |
| `fts_conversation`      | Conversation index (ADR-082 §4 step 2, reuses same machinery) |
| `fts_sessions`          | Session metadata full-text index (title/agent_id/workspace) |

**Why trigram**: under `unicode61`, CJK text collapses into a single token; cross-word matches miss. `trigram` slices into 3-character windows, aligning substring matches with human expectations (ADR-082 D3).

### 3.3 `nodes` row contract

- `id INTEGER PRIMARY KEY` — in-DB primary key; Runtime/distiller cross-node association uses this ID.

> ⚠️ **id domain**: historically the docs mixed `node_id: String` / `entity_id: u64`. **Current contract**:
> `nodes.id` is `INTEGER` (u64 externally); the `id` field in the node's JSON serialization is `Option<u64>` at the trait layer (serde projection). Runtime business logic uniformly uses `u64`. **String node IDs are no longer used** (any historical String reference predates it; should be cleaned up in code review).

- `label TEXT` — one of `{Episodic, Knowledge, Procedural, Autobiographical, Session, Conversation}`.
- `status TEXT` — node lifecycle state `{Active, Dormant, Archived}`; default `Active`.
- `props JSON NOT NULL` — `serde_json` serialization of the label-specific node struct (every field except `id` / `embedding`).
- `created_at` / `updated_at` — ISO8601 strings; default is empty string (not NULL).

### 3.4 `vectors` row contract

```
node_id   INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE
dim       INTEGER NOT NULL        -- current in-library vector dimension
embedding BLOB NOT NULL           -- dim × f32 little-endian
```

A dimension mismatch on write errors out — silent score corruption is never acceptable; a stale caller dimension is overridden by the stored `meta.embedding_dim` (see `SqliteStore::from_connection` priority).

### 3.5 `edges` row contract (application-layer graph traversal)

```
src_id    INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE
dst_id    INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE
kind      TEXT NOT NULL           -- {PREFERS, RELATES_TO, CONTRADICTS, ...}
weight    REAL NOT NULL DEFAULT 1.0
props     JSON                    -- edge properties (e.g. timestamp, source)
```

### 3.6 `purge_log` row contract

Forgetting is the only path that destroys node data — archive first, then delete (`SqliteStore::purge_expired`):

```
id        INTEGER PRIMARY KEY
node_id   INTEGER NOT NULL
label     TEXT NOT NULL
props     JSON NOT NULL         -- full row JSON, recovery = re-INSERT
content   TEXT NOT NULL DEFAULT ''
reason    TEXT NOT NULL DEFAULT '' -- {decay, manual_purge, conflict_lost, ...}
purged_at TEXT NOT NULL
```

---

## 4. Schema Evolution (ADR-082 D2)

### 4.1 The trinity

```rust
pub const SCHEMA_VERSION: i64 = 1;                           // current version
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[...];        // [(target, sql)]
pub(crate) fn apply_migrations(conn: &mut Connection) -> Result<()>;
```

### 4.2 Upgrade protocol

`PRAGMA user_version` is the single source of truth for the schema version. `apply_migrations` flow:

1. Read stored value from `PRAGMA user_version`.
2. For each `(target, sql)`, execute only when `stored < target`.
3. After all steps run, write `PRAGMA user_version = SCHEMA_VERSION` inside a single transaction.

**Idempotency**: each migration step's SQL guards itself with `WHERE NOT EXISTS` / `IF NOT EXISTS`, so repeated execution never errors.

### 4.3 Adding a column (developer reference)

1. Add a new entry `(target = SCHEMA_VERSION + 1, sql)` to `MIGRATIONS`, with `ADD COLUMN ... WHERE NOT EXISTS`.
2. Add the same column to the initial DDL in `SCHEMA_SQL` so a fresh DB gets it without migration.
3. Add the field to the node struct with `#[serde(default)]` so old databases still deserialize.
4. Run `tests::round_trip_*` to assert the new field round-trips.
5. Open PR, bump `SCHEMA_VERSION`.

> `acowork-sqlite::SCHEMA_VERSION` is independent of the trait-layer semantic version — it is the SQLite in-library `user_version`, decoupled from the `acowork-memory` crate version.

---

## 5. SqliteStore — MemoryProvider Implementation

### 5.1 Factories

```rust
// Main path (embedding provided by caller)
SqliteStore::open(path: impl AsRef<Path>, embedding_dim: usize) -> Result<Self>

// In-memory (tests)
SqliteStore::open_in_memory(embedding_dim: usize) -> Result<Self>

// Dimension-agnostic open (subsystems sharing the workspace .sqlite file that
// never touch vectors — SessionMeta / Conversation)
SqliteStore::open_dim_agnostic(path: impl AsRef<Path>) -> Result<Self>
```

Why `open_dim_agnostic` exists: subsystems sharing the workspace `.sqlite` file (SessionMeta, Conversation) must not race to write `meta.embedding_dim`; if they `open(path, 768)` first, the later `open(path, 384)` memory subsystem would inherit the wrong 768 (see the `meta`-priority logic in `lib.rs::from_connection`).

### 5.2 Concurrency model

- Single `Connection` with outer `Mutex`.
- ADR-082 workload premise: **one agent per DB, zero query concurrency**; serial access is sufficient and simpler.
- `Connection: Send`, so `SqliteStore: Send + Sync` (statically asserted).
- If high concurrency arises in the future → swap to a pool, **without** changing the trait interface.

### 5.3 Trait method → implementation map

| `MemoryProvider` method family                  | `acowork-sqlite` implementation location |
| ----------------------------------------------- | ----------------------------------------- |
| Episodic CRUD / retrieval                         | `provider.rs::episodic_*`                 |
| Consolidated CRUD / retrieval                    | `provider.rs::consolidated_*`             |
| Hybrid retrieval / RRF fusion                    | `retrieval.rs` + `provider.rs`            |
| Node CRUD / edge CRUD                            | `provider.rs::node_*` / `edge_*`          |
| Forgetting decay / archive                       | `provider.rs::decay_*` / `purge_*`        |
| Health check / stats                            | `provider.rs::health_*`                   |
| `MemoryAdminService` (node list / rebuild)        | `admin.rs`                                |

### 6. Retrieval Metrics (`acowork-memory::retrieval_metrics.rs`)

`RetrievalMetrics` collects offline evaluation and online metrics:

| Metric        | Meaning                                                                  |
| ------------- | ------------------------------------------------------------------------ |
| Abstention    | Refuse to answer when retrieval is unreliable (paired with ADR-082 P6 `AbstentionConfig`) |
| Conflict      | Conflict-arbitration accuracy (paired with ADR-068 Step 4 Judge)          |
| Dedup         | De-duplication of high-similarity nodes with the same `(subject, predicate)` (ADR-062 D2) |
| LongMemEval   | LongMemEval subset regression metrics                                    |

**Decoupled from the trait**: collection points live as hooks inside `MemoryManager`, not in the trait; swapping storage backends does not affect metrics.

---

## 7. Key Invariants of `SqliteStore`

- **Symmetric round-trip**: nodes round-trip through `serde_json` field-for-field equal (asserted by `tests::tests::round_trip_*`).
- **Dimension consistency**: `vectors.dim` matches `meta.embedding_dim`; reopen honors the stored value.
- **FK cascades**: `vectors` / `purge_log` FKs are all `ON DELETE CASCADE`; deleting a node cleans up resources.
- **Status projection**: `status` never goes into `props`, always in `nodes.status` column for cheap queries.
- **WAL safety**: `PRAGMA synchronous = NORMAL` + WAL; crash-recovery safe (see ADR-082 D2).

---

## 8. Error Type

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]     Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]       Json(#[from] serde_json::Error),
    #[error("io: {0}")]         Io(#[from] std::io::Error),
    #[error("{0}")]             Memory(String),  // invariant violation
}
```

`From<Error> for AcoworkError::Memory` is implemented; `?` propagates directly inside trait implementations.

---

## 9. Boundaries with Neighbouring Modules

| Boundary             | ↔                                                                                                          | Notes                                                                                  |
| -------------------- | --------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Runtime ↔ storage    | ↔                                                                                                         | Runtime holds only `Arc<dyn MemoryProvider>`, never `use acowork_sqlite::*`           |
| Runtime ↔ embedding  | ↔                                                                                                         | Runtime generates vectors via `EmbeddingProvider` trait; Store never holds the provider |
| Storage ↔ workspace  | ↔                                                                                                         | Store writes `<install_path>/memory/private.sqlite` (+ WAL/SHM); Gateway does not access directly |
| Cloning              | ↔                                                                                                         | `acowork-sign::clone` copies `private.sqlite` + `*.wal` + `*.shm` triplet (ADR-082 D5) |
| InboxAgent tests     | ↔                                                                                                         | Integration tests use `SqliteStore::open_in_memory()`, same trait / same path as production |

---

## 10. Deprecated Capabilities (Do Not Reintroduce)

These capabilities existed in the `acowork-grafeo` era but have been removed after ADR-082 implementation. **New code must not call them**:

- ❌ `grafeo-engine` crates (grafeo / grafeo-common / grafeo-core) — removed from workspace.
- ❌ `grafeo-engine` native LPG / GQL / HNSW / BM25 / PageRank / CDC / community-detection APIs.
- ❌ `MemoryProvider::graph_expand_*` / `create_memory_edge` / `apply_pagerank_boost` methods (removed by ADR-082 D4).
- ❌ `.grafeo` single-file storage — historical working-tree residue (`memory/private.grafeo*`) can be physically deleted.
- ❌ `meta/*.json` sidecar files — SessionMeta data has migrated to the `sessions` table.
- ❌ `conversation_index.grafeo` / standalone `conversation_index.sqlite` — all unified into the `nodes` table.
