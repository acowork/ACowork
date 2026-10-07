# acowork-memory + acowork-sqlite — Agent 私有记忆引擎

**定位**：Agent 私有 Memory 的存储 + 检索 + 离线蒸馏 双 crate 描述。

- `acowork-memory`：记忆 trait / 类型 / 管理器 / 离线蒸馏 / 检索质量指标 —— **存储无关**。
- `acowork-sqlite`：唯一生产存储后端，单文件 SQLite（`memory/private.sqlite`），提供 `SqliteStore` 实现 `acowork_memory::MemoryProvider`。

`MemoryProvider` trait（ADR-051）由 Runtime 通过 `Arc<dyn MemoryProvider>` 持有，Runtime 不接触具体存储引擎。

---

## 1. 设计目标与边界

| 维度              | 目标 / 边界                                                                          |
| ----------------- | ------------------------------------------------------------------------------------ |
| 存储引擎          | 单文件 SQLite（rusqlite 直连，无 ORM，无 Diesel）。PRAGMA WAL + NORMAL + foreign_keys |
| 向量检索          | 同库 `vectors` 表 + 应用层余弦扫描；当前实现是全量精确扫描，详见 ADR-082 C1         |
| 全文检索          | 每 Label 一张 FTS5 虚表，`tokenize='trigram'`（CJK 子串匹配）                          |
| 知识图谱          | 应用层 `edges` 表存关系，`nodes` 表存实体；图遍历 = SQL JOIN + 早停                     |
| Embedding          | 来自 Runtime `EmbeddingProvider` trait（Ollama / Remote 降级），Store 不持有           |
| 生命周期          | 启动 O(1)（无 WAL replay、无 BM25 rebuild），详见 ADR-082 §3                         |
| Schema 演进       | `PRAGMA user_version` + 幂等迁移步，详见 ADR-082 D2                                  |
| 隔离              | 进程内 `MemoryProvider` 实现按 workspace/agent 单文件，详见 ADR-009                  |

---

## 2. Crate 结构

### 2.1 `core/acowork-memory/`

```
crates/acowork-memory/
├── Cargo.toml
└── src/
    ├── lib.rs                  # Pub re-exports of trait/types/managers
    ├── provider.rs             # MemoryProvider trait (35+ methods)
    ├── store.rs                # MemoryStore trait (legacy, 16 methods)
    ├── manager.rs              # MemoryManager — Retrieve / Inject / Record 三阶段
    ├── quality.rs              # MemoryQualityConfig + DedupQuality + ConsolidationQuality
    ├── admin.rs                # MemoryAdminService — 节点 CRUD / 列表 / 重建
    ├── keyword.rs              # 关键词 sanitize / 长度门禁 (ADR-062)
    ├── judge.rs                # LLM Judge 采样决策 (ADR-068 §5)
    ├── session_meta.rs         # SessionMeta + SessionMetaStore trait + TodoItem
    ├── types.rs                # Episode / KnowledgeNode / ProceduralNode
    │                           # AutobiographicalNode / MemoryQuery / SearchResult
    │                           # MemoryContext / DecayConfig / DecayScanResult ...
    ├── consolidation/
    │   ├── mod.rs              # EpisodicDistiller trait + SchedulerConfig
    │   └── distiller.rs        # DefaultEpisodicDistiller（服务端 LLM 蒸馏实现）
    └── retrieval_metrics.rs    # 检索质量指标（Abstention / 冲突 / 去重维度评估）
```

**模块职责一句话**：

- `provider.rs` / `store.rs`：定义 trait（实现侧在 `acowork-sqlite`）。
- `manager.rs`：Runtime 与 Store 之间的中间层，编排三阶段记忆生命周期。
- `consolidation/distiller.rs`：离线蒸馏 6 步流水线（ADR-068 §3.4），按 `MemoryProvider` 接口做写读。
- `retrieval_metrics.rs`：检索质量指标（见 §6）。
- `quality.rs` / `keyword.rs` / `judge.rs`：蒸馏侧门控配置。

### 2.2 `core/acowork-sqlite/`

```
crates/acowork-sqlite/
├── Cargo.toml
└── src/
    ├── lib.rs                  # SqliteStore struct + 错误类型 + 顶层工厂
    ├── schema.rs               # SCHEMA_SQL + SCHEMA_VERSION + MIGRATIONS + apply_migrations
    ├── provider.rs             # MemoryProvider / MemoryStore trait 实现（~800 行核心）
    ├── retrieval.rs            # hybrid_search / vector_search / text_search / RRF_K
    ├── admin.rs                # MemoryAdminService 实现（节点浏览 / 列表 / 重建）
    ├── conversation.rs         # ConversationStore（ADR-082 §4 step 2，复用 nodes/vectors/FTS）
    ├── session_meta.rs         # SqliteSessionMetaStore（ADR-082 §4 step 3，sessions 表）
    └── tests.rs                # trait round-trip 断言（每类节点字段必现）

tests/
├── memory_chains_e2e.rs        # 写读 / 蒸馏 / 去重 端到端
├── shared_store_e2e.rs         # 多实例共享同库（kafka 风格多 label 边界）
├── session_meta.rs             # sessions 表 CRUD
└── store_isolation.rs          # 同库不同 label 隔离（privacy 边界）
```

---

## 3. SQLite Schema（ADR-082 D1）

单库 8 张物理表 + 6 张虚表，所有 DDL 在 `schema.rs::SCHEMA_SQL` 内幂等；任何次 `open` 重跑不破坏数据。

### 3.1 物理表

| 表名           | 作用                                                                                     |
| -------------- | ---------------------------------------------------------------------------------------- |
| `nodes`        | 全部记忆节点的统一行（一行一节点），`label` 区分记忆类型，`props` JSON 存其余字段             |
| `vectors`      | 节点 embedding（f32 BLOB），FK `node_id` → `nodes.id` ON DELETE CASCADE                  |
| `meta`         | 库级元数据（当前仅 `embedding_dim`）；reopen 以库内值为准，避免调用方维度陈旧             |
| `edges`        | 沉淀层节点间关系（应用层图遍历用），`(src_id, dst_id, kind, weight, props)`                  |
| `purge_log`    | 遗忘衰减最终目的表（先归档再删除，单字段 `decay/purge/expiry` 可区分原因）                  |
| `sessions`     | Session 元数据（ADR-082 §4 step 3，替代 `conversations/meta/*.json` 边车文件）             |

### 3.2 虚表（FTS5，trigram 分词）

| 虚表                  | 用途                                              |
| --------------------- | ------------------------------------------------- |
| `fts_episodic`        | 经历层 `content` 全文索引（CJK 子串匹配）         |
| `fts_knowledge`       | 沉淀层 Knowledge `content` 全文索引               |
| `fts_procedural`      | 沉淀层 Procedural `content` 全文索引              |
| `fts_autobiographical`| 沉淀层 Autobiographical `content` 全文索引        |
| `fts_conversation`    | 对话索引（ADR-082 §4 step 2，复用同套机制）       |
| `fts_sessions`        | Session 元数据全文索引（title/agent_id/workspace） |

**trigram 选择理由**：CJK 在 `unicode61` 下会被折叠成一个 token，跨词匹配不到；`trigram` 切成 3 字符窗口，子串匹配对齐人类预期（ADR-082 D3）。

### 3.3 `nodes` 行契约

- `id INTEGER PRIMARY KEY` —— 库内主键；Runtime/蒸馏端跨节点关联走该 ID。

> ⚠️ **id 域**：历史上文档存在 `node_id: String` / `entity_id: u64` 混用。**当前 contract**：
> 库内 `nodes.id` 是 `INTEGER`（u64 对外呈现）；节点 JSON 序列化的 `id` 字段在 trait 层是 `Option<u64>`（serde 投影），Runtime 业务逻辑统一按 `u64` 处理。**不再使用 String 节点 ID**（任何历史 String 引用都是 ADR-082 之前的遗留，应在代码 review 时清理）。

- `label TEXT` —— `{Episodic, Knowledge, Procedural, Autobiographical, Session, Conversation}` 之一。
- `status TEXT` —— 节点生命周期状态 `{Active, Dormant, Archived}`；默认 `Active`。
- `props JSON NOT NULL` —— 该 label 节点结构体的 `serde_json` 序列化（除 `id` / `embedding` 外所有字段）。
- `created_at` / `updated_at` —— ISO8601 字符串，`Default::default()` 是空串（不是 NULL）。

### 3.4 `vectors` 行契约

```
node_id   INTEGER PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE
dim       INTEGER NOT NULL        -- 当前库内向量维度
embedding BLOB NOT NULL           -- dim × f32 little-endian
```

写入时维度校验失败 → 报错，不静默破坏相似度分数；调用方陈旧维度被库内值覆盖（`SqliteStore::from_connection` 优先 `meta.embedding_dim`）。

### 3.5 `edges` 行契约（应用层图遍历）

```
src_id    INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE
dst_id    INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE
kind      TEXT NOT NULL           -- {PREFERS, RELATES_TO, CONTRADICTS, ...}
weight    REAL NOT NULL DEFAULT 1.0
props     JSON                    -- 边属性（如时间戳、来源）
```

### 3.6 `purge_log` 行契约

遗忘是唯一销毁节点数据的路径，先归档再删除（`SqliteStore::purge_expired`）：

```
id        INTEGER PRIMARY KEY
node_id   INTEGER NOT NULL
label     TEXT NOT NULL
props     JSON NOT NULL         -- 整行 JSON，恢复 = 重新 INSERT
content   TEXT NOT NULL DEFAULT ''
reason    TEXT NOT NULL DEFAULT '' -- {decay, manual_purge, conflict_lost, ...}
purged_at TEXT NOT NULL
```

---

## 4. Schema 演进（ADR-082 D2）

### 4.1 三件套

```rust
pub const SCHEMA_VERSION: i64 = 1;                           // 当前版本
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[...];        // [(target, sql)]
pub(crate) fn apply_migrations(conn: &mut Connection) -> Result<()>;
```

### 4.2 升级协议

`PRAGMA user_version` 是 schema-version 的唯一真相。`apply_migrations` 流程：

1. `PRAGMA user_version` 读出 stored 值；
2. 对每个 `(target, sql)`，`stored < target` 才执行；
3. 全部执行后写 `PRAGMA user_version = SCHEMA_VERSION`，整体一个 transaction。

**幂等保证**：每个迁移步的 SQL 自身用 `WHERE NOT EXISTS` / `IF NOT EXISTS` 守卫，重复执行不报错。

### 4.3 加字段流程（开发者参考）

1. 在 `MIGRATIONS` 加新条目 `(target = SCHEMA_VERSION + 1, sql)`，SQL 中 `ADD COLUMN ... WHERE NOT EXISTS`；
2. 同步把列加进 `SCHEMA_SQL` 的初始 DDL（让 fresh DB 直接有列，无需迁移）；
3. 节点结构体加 `#[serde(default)]` 字段（保证旧库读不报错）；
4. 跑 `tests::round_trip_*` 断言新字段 round-trip 不丢；
5. 提 PR，bump `SCHEMA_VERSION`。

> `acowork-sqlite::SCHEMA_VERSION` 与 trait 层语义版本独立 —— 它是 SQLite 库内 `user_version`，与 `acowork-memory` crate 版本号无耦合。

---

## 5. SqliteStore —— MemoryProvider 实现

### 5.1 工厂函数

```rust
// 主路径（embedding 由调用方传入）
SqliteStore::open(path: impl AsRef<Path>, embedding_dim: usize) -> Result<Self>

// 内存版（测试）
SqliteStore::open_in_memory(embedding_dim: usize) -> Result<Self>

// 不写 embedding_dim 的开口（SessionMeta / Conversation 等共享库文件、不碰向量的子系统）
SqliteStore::open_dim_agnostic(path: impl AsRef<Path>) -> Result<Self>
```

`open_dim_agnostic` 存在的原因：共享 workspace `.sqlite` 文件的子系统（SessionMeta、Conversation）不应抢着写 `meta.embedding_dim`；如果它们先 `open(path, 768)` 落了维度，再来 `open(path, 384)` 的真记忆子系统会沿用错误的 768（详见 `lib.rs::from_connection` 的 `meta` 优先逻辑）。

### 5.2 并发模型

- 单 `Connection`，外层 `Mutex` 保护。
- ADR-082 工作负载前提：**一 agent 一库，零查询并发**；串行访问充分且简单。
- `Connection: Send`，`SqliteStore: Send + Sync`（静态断言）。
- 高并发场景（如果将来发生）→ 升��路径：换连接池，**不改** trait 接口。

### 5.3 trait 实现的对应关系

| `MemoryProvider` 方法族                  | `acowork-sqlite` 实现位置       |
| ---------------------------------------- | ------------------------------- |
| 经历层 CRUD / 检索                       | `provider.rs::episodic_*`       |
| 沉淀层 CRUD / 检索                       | `provider.rs::consolidated_*`   |
| 混合检索 / RRF 融合                      | `retrieval.rs` + `provider.rs`  |
| 节点 CRUD / 边 CRUD                      | `provider.rs::node_*` / `edge_*` |
| 遗忘衰减 / 归档                          | `provider.rs::decay_*` / `purge_*` |
| 健康检查 / 统计                          | `provider.rs::health_*`         |
| `MemoryAdminService`（节点列表/重建）    | `admin.rs`                        |

### 6. 检索指标（acowork-memory::retrieval_metrics.rs）

`RetrievalMetrics` 收集离线评估与在线指标：

| 维度          | 含义                                                                 |
| ------------- | -------------------------------------------------------------------- |
| Abstention    | 检索不可靠时拒答（与 ADR-082 P6 的 `AbstentionConfig` 配对）          |
| Conflict      | 冲突仲裁准确率（与 ADR-068 Step 4 Judge 配对）                         |
| Dedup         | 同 `(subject, predicate)` 高相似节点的去重（ADR-062 D2）               |
| LongMemEval   | LongMemEval 子集回归指标                                            |

**与 trait 解耦**：指标采集点放在 `MemoryManager` 内的 hook，不进 trait；切换存储后端不影响指标。

---

## 7. SqliteStore 关键不变量

- **写读对称**：节点 `serde_json` round-trip 后字段全等（`tests::tests::round_trip_*` 断言）。
- **维度一致**：`vectors.dim` 与 `meta.embedding_dim` 一致；reopen 以库内值为准。
- **FK 级联**：`vectors` / `purge_log` FK 全 `ON DELETE CASCADE`，节点删除时资源随之清理。
- **Status 投影**：`status` 不进 `props`，总是 `nodes.status` 列，便于过滤。
- **WAL 不丢**：`PRAGMA synchronous = NORMAL` + WAL，崩溃恢复安全（详见 ADR-082 D2）。

---

## 8. 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]     Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]       Json(#[from] serde_json::Error),
    #[error("io: {0}")]         Io(#[from] std::io::Error),
    #[error("{0}")]             Memory(String),  // 不变量违反
}
```

`From<Error> for AcoworkError::Memory` 已���现，`?` 在 trait 实现内直接传播。

---

## 9. 与相邻模块的边界

| 边界                    |     | 说明                                                                                  |
| ---------------------- | --- | ------------------------------------------------------------------------------------- |
| Runtime ↔ emulator   | ↔   | Runtime 仅持 `Arc<dyn MemoryProvider>`，从不 `use acowork_sqlite::*`                 |
| Runtime ↔ Emedding    |     | Runtime 通过 `EmbeddingProvider` trait 生成向量，Store 不持有                          |
| Storage ↔ Workspace   |     | Store 写 `<install_path>/workspace/memory/private.sqlite`（含 WAL/SHM），Gateway 不直访         |
| Cloning                |     | `acowork-node` 的 clone 拷贝 `private.sqlite` + `*.wal` + `*.shm` 三件套（ADR-082 D5） |
| InboxAgent 测试         | ↔   | 集成测试用 `SqliteStore::open_in_memory()`，与生产代码同 trait、同路径                  |

---

## 10. 已下线能力（避免重新引入）

下面这些能力在 `acowork-grafeo` 时代存在，但 ADR-082 实施后已下线。**新代码不应再调用**：

- ❌ `grafeo-engine` crates（grafeo/gradfo-common/gradfo-core）—— crate 已从 workspace 删除。
- ❌ `grafeo-engine` 原生 LPG / GQL / HNSW / BM25 / PageRank / CDC / 社区检测 API。
- ❌ `MemoryProvider` 的 `graph_expand_*` / `create_memory_edge` / `apply_pagerank_boost` 方法（ADR-082 D4 删除）。
- ❌ `.grafeo` 单文件存储 —— 历史工作树残留（`memory/private.grafeo*`）可物理删除。
- ❌ `meta/*.json` 边车文件 —— SessionMeta 数据已迁移进 `sessions` 表。
- ❌ `conversation_index.grafeo` / 独立 `conversation_index.sqlite` —— 全部进 `nodes` 统一表。