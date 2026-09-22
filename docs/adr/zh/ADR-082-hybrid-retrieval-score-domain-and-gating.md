# ADR-082：混合检索的分数域与门控（各源把门 + 名次融合）

**状态**：已实施（2026-09）
**日期**：2026-09
**决策者**：大鱼
**取代**：[ADR-062](./ADR-062-memory-quality-config-and-retrieval-gate.md) §6.4（`auto_inject` 的 `min_score`）与 §2.4 参数表中的 `min_score` 行——本文以 `min_cosine`（余弦绝对域）取代"融合分阈值"
**前置 ADR**：ADR-051（Runtime / Grafeo 解耦）、ADR-057（压缩蒸馏入图）、ADR-062（MemoryQualityConfig）、ADR-081（全局搜索，消费方）
**关联设计**：[05-memory.md §6.5 / §6.6](../../design/zh/05-memory.md)

---

## 1. 背景与问题

`memory_recall` 在真实 agent 上**返回 0 条**（中文查询必现），而库中确有高度相关的节点。根因既不是 embedding 缺失，也不是索引没建：**是门控挂错了分数域**。

### 1.1 调用链

```
memory_recall 工具
  → MemoryManager::retrieve            (acowork-memory/src/manager.rs)
  → MemoryProvider::hybrid_search_full (acowork-grafeo/src/provider_impl.rs)   ← 门控曾在此
  → GrafeoStore::hybrid_search         (引擎 hybrid_search → fuse_results)
```

### 1.2 决定性事实：同一个 `score` 字段承载三种量纲

- `grafeo-engine-0.5.42/src/database/search.rs:385` `hybrid_search`：收集**所有非空源**（BM25 文本索引、向量索引按 `-distance` 取负使"越近分越高"）。
- `grafeo-core-0.5.42/src/index/text/fusion.rs:58` `fuse_results`：
  - `sources.len() == 1` → **原样返回该源的原始分数**（不做 RRF）；
  - `sources.len() == 2` → 走 RRF（`Σ 1/(60+rank)`，恒正）。

| 命中路径 | `score` 实际含义 | 取值范围 |
| --- | --- | --- |
| 文本 + 向量双源 | RRF 名次分 `Σ 1/(60+rank)` | ≈ `[0.016, 0.033]`，**恒正** |
| 仅向量源 | 该源原始分 `-distance = cos − 1` | `[-2, 0]`，**恒非正** |
| 仅文本源 | BM25 分（含 IDF） | 随语料漂移 |

### 1.3 缺陷

旧 `hybrid_search_full` 对**融合分**做 `filter(score >= min_score)`，`min_score` 默认 `0.0`。在"仅向量源"路径下 `score ∈ [-2, 0]`，于是 `score >= 0.0` 等价于**要求 `cos >= 1`**——连浮点误差下的完美匹配（`score = -1.19e-7`）都被滤掉。

**中文查询 BM25 命中 0 条 → 必然走纯向量路径 → 必然返回 0 条。** 含 ASCII 的查询 BM25 命中 → 走 RRF → 正分 → 通过。这精确解释了线上"中文 0 条、英文 3 条"的现象。

### 1.4 实测证据（真实库副本，Episodic 132 节点 / 131 带向量）

```
text_search (BM25) "PONYTAIL_EMBED_PROBE_7F3A" -> 5 hits
text_search (BM25) "独角兽冰淇淋彩虹气球"        -> 0 hits      ← 中文 BM25 完全失效
```

```
[中文查询 × 探针向量]  融合分 = [-1.19e-7, -0.324, -0.362]   全部 ≤ 0  → 旧门控滤光 → 0 条
                       min_cosine=0.3 → [0.9999, 0.838, 0.819]            → 3 条 ✅
[中文查询 × 无关向量]  融合分 = [-0.0117, -1.037, -1.039]
                       min_cosine=0.3 → [0.994]
[ASCII × 探针向量]     融合分 = [0.0328, 0.0323, 0.0313]     恒正（RRF）→ 旧门控恰好通过
```

---

## 2. 决策

### D1：各源各自把门，取并集

```rust
保留集 = (向量源命中 且 cos ≥ min_cosine) ∪ (文本源命中)
```

- **向量源**按绝对余弦相似度（`cos = 1 − distance`，归一化为 `(1 + cos)/2 ∈ [0,1]`）过滤；
- **文本源**按 BM25 相关性（不设固定阈值，见 §3.1）；
- **两源存活者取并集**后再排序。

被文本源命中的候选**即使 embedding 远离也必须保留**——词法命中是它自己的依据；否则弱/退化 embedding 会连带把 BM25 命中一起静默杀掉（本仓曾出现：确定性哈希 fallback 向量下 cos ≈ 0，`ADR-062` M4/M5 benchmark 指标全归零）。

### D2：阈值定义在余弦绝对域

`MemoryQualityConfig.min_cosine: f32`，默认 `0.3`，经 manifest `[memory.quality] min_cosine` 可覆盖。原 `min_score` 字段与 `apply_min_score` 全部删除（含 trait 签名、manager 传参、7 处调用点）。已部署 manifest 的 `min_score` 键因 `#[serde(default)]` 被忽略，不会导致反序列化失败。

### D3：排序只用名次

跨源排序**不做归一化、不比较数值大小**，只按引擎的 RRF 名次（`k = 60`）。理由见 §3。

### D4：删除一切对融合分的裁剪

融合分不再作为任何阈值、权重缩放的输入。`SearchResult.score` 的语义统一为"归一化余弦相似度 ∈ [0,1]（文本源单独存活者为中性 0.5，表示无语义相似度信息）"。

---

## 3. 为什么不"归一化后混排"

这是本 ADR 的核心论证：**三种分数没有共同量纲，且 RRF 本身不需要共同量纲。**

### 3.1 为什么不能给 BM25 挂绝对阈值

BM25 含 IDF 项，分数随语料规模、文档频率、平均文档长度漂移：同一 query 在 100 节点库与 10 万节点库上不可比，固定阈值必然过时。因此文本源**不设默认阈值**——BM25 命中即视为相关，这是保守而正确的选择。

### 3.2 为什么 RRF 分数不能挂阈值

`1/(60+rank)` 只编码名次，不含相关度幅值；且它在**单源路径下不存在**（退化为原始分）。对它的任何绝对阈值都是无意义的。

### 3.3 为什么 cosine 是唯一可挂绝对阈值的量

余弦相似度是绝对量：0.8 在任何查询、任何库里都表示 0.8 的相似度。因此**唯一合法的绝对门控落在向量源的余弦域**。

### 3.4 备选方案：per-query min-max 归一化后加权混排（否决）

`FusionMethod::Weighted` 走的是"每次查询内 min-max 归一化 → 加权求和"。三重缺陷：

1. **伪绝对量**——归一化后的 0.9 只表示"本查询内相对最高"，跨查询不可比，阈值无从设定；
2. **离群值敏感**——单个极值即压缩其余分布；
3. **单命中退化**——只有一条结果时归一化无定义。

代码中 `hybrid_search_weighted` 的原注释已记录该结论：*"Weight scaling after RRF is meaningless… removed to avoid confusion"*。

### 3.5 结论

**"混排"的合法实现就是：过滤在各源域内独立完成，排序只消费名次。** 这不是"各自排"，也不是"归一化后比大小"——是"各自把门 + 名次融合"。这也是 Elasticsearch 8.8+ `rrf` retriever、Weaviate `rankedFusion`、Qdrant、Vespa 的主流做法（RRF `k=60` 为业界默认）。

---

## 4. 实施与验收

### 4.1 代码改动

| 文件 | 改动 |
| --- | --- |
| `acowork-memory/src/quality.rs` | `min_score` → `min_cosine`（默认 0.3）+ `From<ManifestMemoryQuality>` 映射 |
| `acowork-memory/src/types.rs` · `provider.rs` · `manager.rs` | 查询字段 / trait 签名 / 传参改名 |
| `acowork-core/src/manifest.rs` | `ManifestMemoryQuality.min_cosine` |
| `acowork-grafeo/src/provider_impl.rs` | `hybrid_search_full` 重写为 D1 的"各源把门 + 并集" |
| `acowork-grafeo/src/retrieval.rs` · `grafeo.rs` | 删除 `apply_min_score` 及全部融合分裁剪 |
| 测试与工具 | abstention 测试改用正交 embedding 表达"不相关"；新增 cos 门槛回归测试；`diagnose_memory` / `verify_real_stores` 诊断工具 |

### 4.2 验收结果

- `cargo clippy --workspace --lib --tests --examples -- -D warnings` → 0 警告；`cargo build` 全 crate 通过。
- 测试：grafeo lib **218** / memory lib **33** / runtime lib **1509** 全绿。
- **ADR-062 M4 benchmark 恢复且提升**：Precision@5 `0.57 → 0.80`、MRR `0.80 → 0.90`、Dormant 垃圾比 `0.30 → 0.00`；**M5**：keyword hit@5 `0.00 → 1.00`。
- 真实库副本：修复前返回 0 条的中文查询，现返回归一化余弦分 `[0.9999, 0.838, 0.819]`；无关向量正确不进入结果。

---

## 5. 已知天花板（诚实清单）

| # | 天花板 | 实测证据 | 影响 |
| --- | --- | --- | --- |
| C1 | **绝对余弦地板强依赖 embedding 提供方** | 真实库上 `min_cosine` 取 `0.0 / 0.3 / 0.6` 命中数**完全相同**（各向异性：不相关文本对仍处 cos 0.5–0.9）；而 procedural 哈希 fallback 下 cos ≈ 0，地板会滤光全部 | 阈值的"去凑数"效果在真实数据上接近于零 |
| C2 | **中文 BM25 失效** | `text_search("独角兽冰淇淋彩虹气球") -> 0 hits`，库中确有该文本节点 | **纯中文查询时混合搜索退化为纯向量**，BM25 那一路等于不存在 |
| C3 | **无 rerank** | 全库无 `rerank` / `cross_encoder` 实现 | 缺主流 RAG 精度最大的一段（典型 +10~25 nDCG@10） |
| C4 | **权重未生效** | `hint_weights` 四档传入 `hybrid_search_full` 后被忽略（RRF 等权 `k=60`）；加权 RRF 曾被显式否决 | 排序不体现查询意图；`design/05-memory.md §6.6` 所述"默认 RRF 权重 vector 0.7 / text 0.3"与实现不符（见 §7 文档修正） |
| C5 | **无可信相关性评估集** | 唯一的真实库数字是 `vector recall 100/100 top-10`——用节点自身向量查自身，属**索引完整性自查，不是相关性指标** | 无法判断质量水位，也无法验证优化收益 |

---

## 6. 后续优化计划

| 阶段 | 内容 | 工期 | 验收标准 |
| --- | --- | --- | --- |
| **P1** | **中文分词**：BM25 索引配 CJK analyzer（字符 bigram 优先，免词典；或 jieba） | 2–3 天 | 纯中文查询 BM25 hits > 0；bench 增加中文查询集 |
| **P2** | **评估体系 + CI 门禁**：200–500 条真实中文标注查询（多级 relevance）、nDCG@10 + Recall@k + MRR + p50/p95 延迟 | 3–5 天 | 每次改动自动回归，有基线可比 |
| **P3** | **cross-encoder rerank**：hybrid 召回 top-50 → ONNX reranker（bge-reranker）→ top-K，复用 `acowork-embed` 既有 ONNX 基建 | 3–5 天 | nDCG@10 提升 ≥ 10%，本地延迟可控 |
| **P4** | **weighted RRF**：`Σ wᵢ/(k+rankᵢ)`，w 取自现有 `hint_weights` | 1–2 天 | Semantic 与 Identity 查询排序产生可测差异 |
| **P5** | **分数中心化**：对语料均值中心化 / 每查询 z-score，取代绝对余弦地板（解 C1）；顺带解锁 Weaviate 式 alpha 线性融合 | 2–3 天 | "不相关不返回"在真实 embedding 上生效 |
| P6 | `check_abstention` 接线（`abstention.rs` 现为死代码，无生产调用点） | 1 天 | 低相关查询触发弃权提示 |

**执行顺序建议**：P1 与 P2 并行起步 → P3 → P4 → P5。理由：P1 修补"哑掉的一半召回"，P2 是"能否判断好坏"的前提（无 P2 则后续每一步都是猜）。

---

## 7. 与既有文档的修正关系

| 文档 | 原表述 | 修正 |
| --- | --- | --- |
| ADR-062 §6.4 | "`min_score: Some(0.3)` 落在 RRF 分数域会过滤掉几乎全部结果" | 该论断仅在**双源 RRF** 下成立；真正的坑是 `min_score = 0.0` 在**单源向量路径**下等价于 `cos >= 1`。本文取代该决策 |
| ADR-062 §2.4 参数表 | `min_score`（RRF 域，默认 0.0） | 改名 `min_cosine`，域改为余弦绝对域，默认 0.3 |
| `design/05-memory.md §6.5` | "`hybrid_search` 返回的结果中所有分数低于 `min_score` 的被过滤" | 改为"各源在各域内过滤后取并集"，并区分 `min_cosine` 与 `AbstentionConfig.default_min_score` |
| `design/05-memory.md §6.6` | "所有检索统一使用默认 RRF 权重（vector: 0.7, text: 0.3）" | 实现为**等权 RRF（k=60）**，权重未接入（见 C4 / P4） |

---

## 8. 开放问题

1. `min_cosine` 的默认值是否应随 embedding 提供方（维度/模型）分档？C1 说明单一默认值在不同提供方下行为差异极大。
2. P5 的中心化基线应在线估计（滚动均值）还是离线固化？在线估计可自适应但引入状态与漂移风险。
3. P3 的 reranker 是否值得引入第二个 ONNX 模型（内存/启动成本 vs 精度收益），需 P2 的评估集给出数据后再定。

---

## 9. 关联文档

- [05-memory.md](../../design/zh/05-memory.md)（§6.5 Abstention、§6.6 检索权重）
- [ADR-062](./ADR-062-memory-quality-config-and-retrieval-gate.md)（本文取代其 §6.4）
- [ADR-062 M4 Benchmark 报告](./ADR-062-memory-quality-benchmark-report.md)（分数域实证来源）
- [ADR-051](./ADR-051-runtime-memory-provider-decoupling.md)、[ADR-081](./ADR-081-global-search.md)
