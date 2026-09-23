# ADR-082：记忆存储后端迁移至 SQLite（纯向量 + FTS，去图化）

**状态**：已定案（待实施）
**日期**：2026-09
**决策者**：大鱼
**前置 ADR**：ADR-051（Runtime / Grafeo 解耦——本文成立的直接前提：engine 只被 `acowork-grafeo` 一个 crate 触碰）、ADR-062（MemoryQualityConfig，其 §6.4 `min_score` 决策由本文 D5 最终取代）、ADR-081（全局搜索 / 对话索引）
**关联设计**：[05-memory.md](../../design/zh/05-memory.md)

---

## 1. 背景与问题

### 1.1 导火索：启动 7-9 秒

SSE agent（424 条记忆 / 15581 条对话消息）启动耗时 7-9s。四次启动实测时间线：

| 阶段 | 耗时 | 说明 |
| --- | --- | --- |
| Phase A（包加载 / HTTP / MQTT） | ~0.6s | 正常 |
| `find_latest_session`（264 个 meta 文件） | 50-75ms（偶发 2.9s） | 冷缓存抖动 |
| 记忆库打开 + 向量恢复（424 条） | ~0.6s | ADR-081 P1-2 的 no-rebuild 优化生效 |
| **`ConversationIndex::open`（对话索引）** | **4735-5019ms，四次稳定** | **主瓶颈** |
| SessionManager + 初始会话 | ~0.3s | 正常 |

### 1.2 根因链：40MB WAL 全量重放（读 grafeo-engine 0.5.42 源码确认）

对话索引容器 46MB + WAL 40MB（批量索引 15581 条消息产生，此后永不缩小）。每次 open：

1. 周期 checkpoint（`checkpoint_timer.rs` → `try_checkpoint`）只调 `flush::flush` 刷容器，**不写 WAL `checkpoint.meta`、不轮转 log**（`flush.rs` 仅 `wal.sync()`）；
2. recovery 无 metadata 时从 seq 0 全量读 WAL（`wal/recovery.rs`：仅跳过 `sequence < cp.log_sequence` 的文件，单文件 seq=0 永不满足）；
3. `apply_wal_records`（`database/mod.rs:1080`）**无 epoch 过滤**，15581 个节点创建 + 2KB 向量属性写入被原样重放——数据早已在容器快照里，纯冗余；
4. 显式 `wal_checkpoint()` 写了 metadata 也无效：单文件未轮转时 `log_sequence=0`，跳过条件 `0 < 0` 不成立；`max_log_size` 默认 64MB 且未在引擎 Config 暴露，40MB 永不轮转。

### 1.3 结构性判断：patch 修不完

引擎是"open 时全量进内存"架构，open 成本 = 容器全量加载 + **BM25 每次重建**（引擎不持久化恢复 postings，`init_schema` / `conversation_index.rs` 每次 open 重建，15k 消息 ~0.3s，10 万条时 2-4s）+ 向量恢复扫描（O(N) 全量读向量属性）。WAL 补丁只消掉其中一项，其余随数据量线性增长。记忆库同病：424 节点对应 9.2MB WAL（~22KB/节点，变更历史所致），2000-4000 节点时 open 将涨到 3-6s。

### 1.4 分数域混域：引擎融合 API 的设计缺陷

`memory_recall` 曾在真实 agent 上中文查询**返回 0 条**。根因是引擎 `hybrid_search` 把三种量纲塞进同一个 `score` 字段（`grafeo-engine-0.5.42/src/database/search.rs:385` + `grafeo-core-0.5.42/src/index/text/fusion.rs:58`）：

| 命中路径 | `score` 实际含义 | 取值范围 |
| --- | --- | --- |
| 文本 + 向量双源 | RRF 名次分 `Σ 1/(60+rank)` | ≈ `[0.016, 0.033]`，恒正 |
| 仅向量源 | 原始分 `-distance = cos − 1` | `[-2, 0]`，恒非正 |
| 仅文本源 | BM25 分（含 IDF） | 随语料漂移 |

对融合分做 `score >= min_score(0.0)` 门控，在"仅向量源"路径下等价于要求 `cos >= 1`——中文查询 BM25 命中 0 条 → 必然走纯向量路径 → 必然被滤光。实测（真实库副本）：`min_cosine=0.3` 修复后中文查询 3 条命中（cos 0.9999/0.838/0.819）。这一缺陷是离开该引擎的第三个独立理由：**融合与门控必须由自己实现，分数域才有结构性保证**（见 D5）。

### 1.5 图层是死代码（实证）

- 真实库快照实测 **edges = 0**（`acowork-grafeo/examples/count_edges.rs`）；
- 生产代码无任何边创建路径：`create_memory_edge` 仅测试调用；`store_episode_with_session`（HAS_MEMORY）无生产调用方；distiller / consolidation / triple_extraction 均不建边（库中 10 个 Session 节点为旧版化石）；
- 但 `enable_graph_expand` 默认 `true`——**每次召回都跑 spreading activation BFS 遍历空图**，必然返回空。spreading.rs、边权重公式、图扩展 dedup 全部在为不存在的图服务。

### 1.6 工作负载前提（设计约束的出发点）

一个 agent 一个库；**零并发**；写入频率：记忆库分钟级、对话库几十秒级。实时写完全无压力。这个场景不需要为高吞吐设计的 WAL 缓冲架构，需要的是"写后即持久、open 只读"。

---

## 2. 决策

### D1：存储后端换 SQLite（rusqlite, bundled）

去掉 grafeo-engine / grafeo-storage / grafeo-core / grafeo-common 四个依赖，换 rusqlite 一个。`GrafeoStore` 的 API 边界保留（ADR-051 的回报），runtime 12 个使用方只 import `GrafeoStore` + 自有类型，无需改动。schema：

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

-- 每个检索字段一张虚表（Knowledge / Procedural / Autobiographical 同理）
CREATE VIRTUAL TABLE fts_episodic USING fts5(
  content, node_id UNINDEXED, tokenize = 'trigram'
);
```

### D2：向量检索 = 内存缓存 + 暴力扫（精确解），放弃 HNSW

- 每 label 懒加载向量到内存缓存（当前 15581 × 2KB = 32MB，顺序读 3-10ms，一次性），查询纯内存 SIMD 余弦，亚毫秒，**recall = 100%**（HNSW 是近似解，典型 recall@10 95-99%）；
- HNSW 正是本次复杂度的根源：索引持久化 / restore / sync / WAL 重放这一整套机器都为"内存图结构重建贵"而生。暴力扫没有任何索引要维护，向量就是表里一行 BLOB；
- **天花板**（`ponytail:`）：10 万条 ≈ 200MB 常驻、单查几毫秒，无感；100 万条 ≈ 2GB 才是动手信号。升级路径被 `retrieval.rs::vector_search` 单函数封死：f16 存储（减半）→ mmap 向量文件 → 进程内 ANN（usearch 等，索引退化为可丢弃缓存，坏了一次 rebuild 即可，最坏回退暴力扫）。向量 BLOB 格式各阶段通用。

### D3：文本检索 = FTS5（trigram 分词），持久索引

- FTS5 索引持久增量维护，**每次 open 重建 BM25 的问题整类消失**；
- 分词用 `trigram`（SQLite ≥3.34）：unicode61 会把连续汉字并成一个长 token（与 §1.4 实测"中文 BM25 完全失效"同类病根）；trigram 支持中文子串匹配（<3 字符查询回退 LIKE）；
- `score = -bm25()`（FTS5 bm25() 负值越小越相关，取负后"越大越好"，与向量域对齐）。

### D4：图层整体删除

删 spreading.rs、`graph_expand` 三处调用、边权重计算（ADR-062 边权重部分）、`edge_types` / `store_edge` / Session-HAS_MEMORY 残留、GQL 中的边查询。**不建 edges 表、不模拟图遍历**。将来真做记忆关联时，SQLite 加一张 edges 表 + 应用层 BFS 是半天工作量——带着真实需求建（YAGNI），且数据模型上随时可加。

### D5：检索分数域与门控（各源把门 + 名次融合，决策保留、实现随迁移重写）

原"混合检索分数域与门控"决策整体并入本文。引擎退役后融合代码自有，§1.4 的混域 bug 结构性消失，门控语义不变：

- **各源各自把门，取并集**：`保留集 = (向量源命中 且 cos ≥ min_cosine) ∪ (文本源命中)`。被文本源命中的候选**即使 embedding 远也必须保留**——词法命中是独立依据，否则弱/退化 embedding 会连带把 BM25 命中一起静默杀掉；
- **阈值定义在余弦绝对域**：`MemoryQualityConfig.min_cosine`，默认 `0.3`，经 manifest `[memory.quality] min_cosine` 覆盖；归一化 `(1 + cos)/2 ∈ [0,1]`。原 `min_score`（融合分域）不复活；
- **文本源不设固定阈值**（BM25 含 IDF 随语料漂移，无稳定绝对域）；
- 融合为等权 RRF（k=60），MMR 重排在向量缓存内两两相似度上实现；
- 现有 `min_cosine` 实现（manifest / provider_impl / abstention）随迁移移植到新融合层，**现有门控测试作为验收标准**。

### D6：写入即持久，open 只读

SQLite WAL 自动管理（默认 Batch 持久化，崩溃最多丢 100ms），checkpoint 由引擎自理。**不需要**应用层写后 checkpoint 挂钩子——这是与 grafeo 方案的本质区别：SQLite 的 open 是 O(1) 懒加载页，与库大小无关；FTS5 索引持久；向量缓存可在后台线程预载。启动时间永久恒定。

---

## 3. 被否决的替代方案

| 方案 | 否决理由 |
| --- | --- |
| grafeo-engine 补丁（rotate-before-checkpoint，2 行） | 只修 WAL 重放一项；BM25 重建、容器全量加载仍随数据量线性增长；第三方引擎语义风险持续（本次事故调查成本已很高） |
| 应用侧写后 `wal_checkpoint()` | 单文件未轮转时 metadata 无效，必须先 rotate，而 rotate 仅由未暴露的 64MB 阈值触发——不可靠 |
| 对话索引关 WAL | 只救得了可重建的对话库；记忆库是主数据，WAL 即持久化保证，救不了 |
| sqlitegraph（oldnordic） | **GPL-3.0-only 一票否决**（静态链接传染）；7 个月 94 版本跨 3 个大版本；无 FTS；本质是"更年轻的 grafeo"，把索引持久化耦合问题原样带回 |
| sqlite-vec / libsql | 当前规模暴力扫即精确解，无需 ANN 扩展；留作 D2 升级路径的候选 |

---

## 4. 迁移计划（三步，每步独立可验证）

1. **双后端**：SQLite 后端实现 `GrafeoStore` 等价 API，现有 188 个测试双后端跑绿；
2. **迁移验证**：SSE 真实数据迁入（记忆节点经 export 路径导入；对话索引从 JSONL 重建——watermark 机制已有），对比检索质量与启动时间；
3. **切换删旧**：runtime 切换，删除 grafeo-engine 依赖、index_persist、spreading / 图层、`migrate_legacy_store` 等（预计净删 ~2k 行）。

---

## 5. 后果

**正面**

- 启动 ~7-9s → **<1s，且 O(1) 永久恒定**，不随数据量退化；
- 中文 BM25 失效（§1.4）在 trigram 分词下一并解决；
- 向量检索从近似变精确；
- 依赖净减 4 crate 换 1；consolidation 批次获得真事务原子性；
- 备份 = `VACUUM INTO` / 文件拷贝，诊断 = 标准 SQLite 工具。

**负面 / 风险**

- 迁移窗口 4-6 天 + 一次性数据迁移；
- 向量常驻内存（10 万条 ≈ 200MB f32；f16 可减半，见 D2 天花板）；
- 失去 grafeo 未来功能（当前无已规划项依赖）；
- FTS5 trigram 对 <3 字符查询需 LIKE 回退（实现注意项）。

---

## 6. 开放问题

1. `props` 用 JSON 列还是按 label 拆宽表？JSON 起步（灵活、够快），出现热路径属性过滤需求再投影列。
2. 向量缓存预载放后台线程的时机（open 后立即 vs 首次检索前）——影响首查延迟 vs 启动 CPU 竞争，迁移时实测定。
3. 对话索引的 watermark 机制原样保留还是改用 SQLite 自增游标？倾向后者（更简单），迁移时定。
4. `min_cosine` 默认值是否应随 embedding 提供方（维度/模型）分档？单一默认值在不同提供方下行为差异较大，待评估集数据。
5. reranker（第二 ONNX 模型）是否引入？内存/启动成本 vs 精度收益，需评估集给出数据后再定。

---

## 7. 关联文档

- [ADR-051](./ADR-051-runtime-memory-provider-decoupling.md)（API 边界——本文可行的前提）
- [ADR-062](./ADR-062-memory-quality-config-and-retrieval-gate.md)（`min_cosine` 语义来源；其 §6.4 `min_score` 决策由本文 D5 最终取代，边权重部分随 D4 废除）
- [ADR-081](./ADR-081-global-search.md)（对话索引；其 index_persist 机制随迁移删除）
- [05-memory.md](../../design/zh/05-memory.md)
