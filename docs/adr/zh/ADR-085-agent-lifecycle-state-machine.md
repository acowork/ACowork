# ADR-085: Agent 生命周期状态机（`ready: bool` → `AgentStatus.state` 枚举）

**状态**：已接受（v2 修订稿，2026-10-01 已实施并通过评审修复——lifecycle 合并进 `AgentStatus`、重连 re-stamp、Gateway 状态码透传、删除 Runtime 抢先建 session；§10 三个开放问题已全部落决策；实施期间的三个落地决策见 §11）
**日期**：2026-09-30
**决策者**：架构评审

**关联**：
- [ADR-033](./ADR-033-mqtt-replace-grpc-websocket.md)（`ready` 主题的最初来源——本 ADR 取代其 plain-text bool 载荷）
- [ADR-038](./ADR-038-session-lifecycle-explicit-model.md)（session 生命周期显式化——本 ADR 是其在上层的镜像）
- [ADR-055](./ADR-055-remote-runtime-node-topology.md)（节点托管 Runtime；`running_agents` 表与 503 反代窗口的来源）
- [ADR-058](./ADR-058-workspace-fs-watcher-mqtt-event.md)（§3.4 明确 Desktop **未订阅** `agents/+/ready` 主题——本 ADR 通过复用 `status` 订阅链路消除该缺口）
- [ADR-065](./ADR-065-unify-mqtt-client-lifecycle.md)（MQTT 客户端生命周期统一；sleep/wake `force_reconnect`——重连不得回退状态这一约束的来源）
- [ADR-073](./ADR-073-agent-instance-identity-decomposition.md)（instance 身份；`status` 主题路径携带 `instance_id`）
- [ADR-076](./ADR-076-multi-user-account-system.md)（多用户；`/latest-session` 404 的两种语义混淆的来源）
- [ADR-082](./ADR-082-memory-storage-sqlite-vector-fts.md)（session-meta 落 SQLite；`/sessions` 的数据源）

---

## 1. 决策摘要

### 1.1 一句话

**废弃 `agents/+/ready` 主题的 plain-text `bool` 载荷，将结构化 `AgentLifecycleState` 枚举合并进现有 `acowork/agents/{instance_id}/status` 主题（`AgentStatus` 新增 `state` / `detail` 字段，不新增主题）**，并让 Runtime 在启动过程的**每个能力边界**上发布权威状态；Runtime 的 session 类接口在子系统就绪前返回 `503 session_not_ready`（而非 `404`），Gateway 反代层**透传** Runtime 状态码（不再把非 2xx 塌缩成 404），使"后端没准备好"与"确实没有数据"在协议层**全链路可区分**。不保留 `ready` 字段，不写兼容代码。

关键认知：**这不只是"加一个字段"，而是承认现有的 `ready` 从来没有过明确定义**。它被两端同时误解——Runtime 在只完成一半时就把 `true` 发出去，Desktop 又把它当成"可以读 session 了"——于是启动窗口内 `404`（"没有 session"）被误读成事实，前端补了一个孤儿 session 进去。

v2 修订的第二个关键认知：**状态载体必须与 LWT 同主题**。MQTT 每个连接只能注册一个 Last Will（当前挂在 `status` 主题，`client.rs:986-994`）。若 lifecycle 独立成主题，进程崩溃时 LWT 只打 `status`，retained lifecycle 会**永远停在 `sessions_ready`**——Desktop 对着一个死进程判定"全部允许"，与本 ADR 要消灭的错误同型。合并进 `AgentStatus` 后，will payload 直接携带 `state=OFFLINE`，retained 状态永不过期失真。

### 1.2 关键决策表（详细理由见 §4）

| # | 决策 | 结论 |
|---|---|---|
| 1 | 状态载体 | **合并进现有 `acowork/agents/{instance_id}/status` 主题**（retained + LWT），`AgentStatus` 新增 `state`（枚举）与 `detail`（字符串）字段，**零新主题、零新编码器**（复用 `encode_agent_status_payload`）。独立 lifecycle 主题方案被否决（§5 方案 G） |
| 2 | 状态取值 | `OFFLINE` / `STARTING` / `HTTP_READY` / `SESSIONS_READY` / `FAILED`（五值 + `UNSPECIFIED`，见 §4.1） |
| 3 | 命名原则 | **按"能做什么"命名，不按"现在是第几步"命名**。禁止 `loading_a` / `loading_b` 这类绑死实现的名字（§4.2） |
| 4 | `ready` 字段 | **废弃**。`AgentListResponse.ready` / `AgentDetailResponse.ready` / `RunningAgentInfo.ready` 一并删除，Desktop 6 处消费点全部改为读 `state`。不保留派生字段 |
| 5 | 兼容 | **不写兼容代码**。`ready` 主题直接删除，Runtime / Gateway / Desktop 必须同版本部署（§4.6） |
| 6 | session 接口 | Runtime 的 `/sessions`、`/sessions/latest`、`/sessions/{sid}/config` 等在 late-bind 槽未填时返回 **`503 {"error":"session_not_ready"}`**；`/sessions/latest` 仅在子系统就绪后才允许 404，且 404 body 统一为 `{"error":"no_session"}`（**含"存在但无权读"场景**——与 ADR-076 §决策 4 的不区分原则一致，避免向非 owner 泄露"该 agent 有 session"） |
| 7 | `latest_session` 读取 | 移除"后台扫描未完成也算 404"的现状：扫描未完成时返回 `503 session_not_ready`，**不猜** |
| 8 | Gateway 反代 | `send_runtime_json` **透传** Runtime 状态码与 body，禁止把非 2xx 塌缩成 404；endpoint 未注册时返回 `503 {"error":"agent_not_running"}`（而非现在的 404）。见 §4.4b |
| 9 | Desktop 订阅 | **无需新增订阅**：`status` 快照已经由 Desktop 的 `agent-event` 通道转发（`workspaceFsEvents.ts` 的 `AgentStatusSnapshot`），`state` 字段随 `AgentStatus` 免费到达。消除"只能靠 30s 轮询看到 ready"的延迟（ADR-058 §3.4 遗留缺口） |
| 10 | 前端决策 | `state < SESSIONS_READY` 时**禁止**调用任何 session 读取/创建接口；`createSession` 的唯一触发条件是"`SESSIONS_READY` + 该账号确实零 session"（§4.5） |
| 11 | 失败态 | 新增 `FAILED`，携带 `detail`（人类可读原因）。`degraded_reasons` 从"只进 `/health`"变为"进 `AgentStatus.detail`"（§4.4） |
| 12 | 重连语义 | **重连不得回退状态**：`STARTING` 仅在进程生命周期内首次连接时发布一次；`run_bootstrap` Step 7 在重连时 re-stamp **当前值**（承接现有 `ready_ever` 机制，见 §4.3b） |
| 13 | initial session | **删除 Runtime 启动时的抢先创建**（`session_init.rs:966-995` else 分支）。零 session 是合法状态（`404 no_session`），session 创建收敛为单写者：仅由 Desktop 在 `SESSIONS_READY` 后经 `POST /sessions` 触发（§4.7） |

### 1.3 不变量（必须满足）

1. **状态由后端权威宣告，前端不得用超时/重试次数推断状态**。前端的能力门控只读 `AgentStatus.state`。
2. **"没准备好"与"没有"在协议层必须可区分**，且该区分必须**贯穿全链路**——Runtime 返回的 503 不得被 Gateway 反代层翻译成 404（§4.4b）。任何"查不到"都不得直接映射为"确实没有"。
3. **`state` 单调前进**：`OFFLINE → STARTING → HTTP_READY → SESSIONS_READY`，`FAILED` 可从任意非 `OFFLINE` 态进入（进程崩溃回 `OFFLINE`）。**禁止** `SESSIONS_READY → HTTP_READY` 回退。**MQTT 重连（sleep/wake、Gateway 重启）不是进程重启，不得回退到 `STARTING`**——重连时 re-stamp 当前值（§4.3b）。回退仅允许发生在进程重启（新 `instance_id`）时。
4. **状态变更必须走 retained 主题，且必须与 LWT 同主题**。故障窗口内 HTTP 可能根本不通（Gateway 反代 503），前端**不能**依赖 HTTP 来发现"我处于什么状态"；进程崩溃时 broker 代发的 will 必须能把状态打回 `OFFLINE`，retained 状态不得 stale。
5. **废弃 `ready` 后不得留残余语义**。不留 `ready` 派生字段，不留"ready 但 sessions 未就绪"这类中间态的兼容分支。
6. **`online` 与 `state` 的一致性**：`state != OFFLINE` 蕴含 `online == true`；LWT 与 Gateway stop 路径同时置 `online=false, state=OFFLINE`。消费端防御性规则：`online == false` 时无论 `state` 为何一律按 `OFFLINE` 处理。
7. **"确保 session 存在"只有一个写者**（Desktop，经 `POST /sessions`）。Runtime 启动路径不得创建 session（§4.7）。

### 1.4 启动状态与能力对照表（故障窗口的完整视图）

| `state` | 进程 | MQTT | HTTP 端口 | `GET /api/agents/{id}/workspaces` | `GET /api/agents/{id}/sessions` | `GET .../latest-session` | Desktop 允许行为 |
|---|---|---|---|---|---|---|---|
| `OFFLINE` | 无/已退 | 断 | — | 503 `agent_not_running` | 503 `agent_not_running` | 503 `agent_not_running` | 显示"未启动"，不调业务接口 |
| `STARTING` | 起 | 未连/刚连 | 未注册 | 503 `agent_not_running` | 503 `agent_not_running` | 503 `agent_not_running` | 显示"启动中"，等 `status` 推送 |
| `HTTP_READY` | 起 | 已连 | 已注册 | **200** | **503 `session_not_ready`** | **503 `session_not_ready`** | 可读 workspaces/config，**禁止**读 session |
| `SESSIONS_READY` | 起 | 已连 | 已注册 | 200 | **200** | **200** 或 `404 no_session` | 全部允许；`404 no_session` 时才 `createSession` |
| `FAILED` | 起/退 | 断或连 | 视情况 | 503 | 503 | 503 | 显示 `detail`，禁止业务接口 |
| `UNSPECIFIED` | — | — | — | 按 `OFFLINE` 门控 | 按 `OFFLINE` 门控 | 按 `OFFLINE` 门控 | UI 区分展示"状态未知（版本不匹配？）"（§10 Q2） |

对照现状：`HTTP_READY` 这一整行在今天的系统里**不存在**——`ready=true` 时 session 接口直接返回含义模糊的 404，前端因此误建 session。

---

## 2. 背景与问题

### 2.1 导火索：删掉的 untitled session 重启后复活

2026-09-30，用户删除 ponytail / software-architect / senior-engineer 三个 agent 的 untitled session，重启后复现。三个 agent 的 SQLite 中各留下一条 0 消息、无标题的孤儿 session：

```
com.acowork.ponytail          -> 20260930_163229_5e1096 (0 msgs)
com.acowork.software-architect -> 20260930_163237_4cde50, 20260930_163027_d99b41
com.acowork.senior-engineer    -> 20260930_163223_3f69ff
```

**删除本身是成功的**：`session_meta.rs` 的 `delete()` 同步执行 `DELETE FROM sessions` + `DELETE FROM fts_sessions`，日志中 `SessionManager: deleted session` 走完整流程。问题不在持久化。

### 2.2 根因：`ready` 的语义未定义，且实现早于其含义

`publish_ready(true)` **全代码库仅一处调用**（`agent_init.rs:582`），位于 Phase A 末尾。Phase B 填充 `session_metadata` / `session_config` 槽后**没有任何代码再次更新状态**。

ponytail 2026-09-30 16:32:28 启动的实测时序（`workspace/logs/20260930_163228.log`）：

```
16:32:28.618  agent_init.rs:582  Phase A ready signal published; Phase B/C continue in background
16:32:29.023  session_init.rs:277  Background session scan complete count=20
16:32:29.058  session_init.rs:968  Initial session created initial_session_id=20260930_162321_353bef
                                       ↑ 440ms 的真空期
```

`ready=true` 宣告后有 **440ms**，session 子系统完全不存在。

### 2.3 故障链（逐跳，每跳都有日志实证）

| # | 时刻 | 事件 | 证据 |
|---|---|---|---|
| 1 | 28.618 | Runtime 发 `ready=true` | `20260930_163228.log:32` |
| 2 | 28.655 | Gateway 侧 `GET /latest-session` → **404**（Runtime 尚未注册 HTTP endpoint，请求**根本没到 Runtime**） | `20260930_163201.log` |
| 3 | 28.669 | `GET /workspaces` → **503**（同一窗口） | 同上 |
| 4 | 28.771 | `GET /sessions?page=1` → **503** | 同上 |
| 5 | 29.845 | `POST /sessions` → **201**，孤儿 session 诞生 | 同上 |
| 6 | 29.936 | Runtime `SessionManager: created new session 20260930_163229_5e1096` | `20260930_163228.log:191` |

第 2 步的 404 尤其恶劣：它来自 **Gateway 侧无 endpoint 时的兜底**，与"agent 确实没有 session"在响应上**完全无法区分**。该 404 的确切产生路径需实施时复核——`proxy_to_runtime` 主路径在 endpoint 缺失时返回 503（`proxy.rs:2518-2530`），而 `send_runtime_json` 在同样条件下返回 **404**（`proxy.rs:2676-2681`），node 兜底路径也有 404（`proxy.rs:1112-1120`）。无论具体来源，暴露的是同一类缺陷：**Gateway 把"后端不可用"映射成了 404**（见 §2.6）。

### 2.4 结构性判断：patch 修不完

现有前端逻辑：

```ts
const latest = await get().fetchLatestSession(id);   // 404 → null
let target = latest?.session_id ?? null;
if (!target) {
  await get().fetchSessions(id);                     // 503 → catch → sessions = []
  target = get().agents[id]?.sessions[0]?.session_id ?? null;
  if (!target) { await get().createSession(id); return; }   // ← 建了孤儿
}
```

三处叠加，任何单点修补都堵不住：

1. `/latest-session` 的 404 混合了"没准备好"与"没有"两种语义；
2. `fetchSessions` 的 catch 把 503 **翻译成空列表**（`agentStore.ts:870-876`），"接口没起来"被误判成"这个账号一条 session 都没有"；
3. `fetchSessionReqId` 是**全局共享**的单调计数器（`agentStore.ts:215`），一个 agent 的慢请求会作废另一个 agent 的响应，进一步放大误判窗口。

**按不变量 1 与 2，这些都不能靠前端重试次数或超时来修**——协议层没有表达"没准备好"的能力，客户端只能猜。

### 2.5 Gateway 注释与实现不符（次要发现，但印证了问题性质）

`dispatch.rs:394-397` 的注释声称 ready 主题在 "Phase A–C have all populated the HTTP server's late-bind slots" 之后发布。实现（`agent_init.rs:582`）在 Phase A 末尾即发布，Phase B/C 在后台继续。**注释描述的是一个比实现更强的契约**——说明 `ready` 的真实含义从未被澄清，文档与代码各说各话。

### 2.6 Gateway `send_runtime_json` 把所有非 2xx 塌缩成 404（v2 新增发现）

`proxy.rs:2701-2707`：凡走 `fetch_runtime_json` / `send_runtime_json` 的 Gateway handler（如 `/api/agents/{id}/conversations/latest`，`chat.rs:58`），**Runtime 返回的任何非成功状态一律被转成 `ApiError::not_found`**；endpoint 未注册也返回 404（`proxy.rs:2676-2681`）。这意味着：即使 Runtime 按本 ADR 改成诚实返回 `503 session_not_ready`，经过这条路径到达前端的仍是 404——**不变量 2 会在 Gateway 层被重新破坏**。该缺陷必须与 D4 同步修复（§4.4b），否则状态机的价值在出 Gateway 的那一刻就被吃掉。

### 2.7 Runtime 启动时抢先创建 initial session（v2 新增发现）

`session_init.rs:966-995`：当无可恢复的 session 时（**恰好等于"用户删光了 session"这个场景**），Runtime 在启动路径里自己 `create_session()`，建出一个 0 消息、无标题、ownerless Private 的 session。后果：

1. "确保 session 存在"出现**第二写者**，与决策 10（Desktop 的 `createSession` 是唯一触发）职责冲突；
2. §8.2 的核心回归断言（"删光 session → 重启 → 0 条 untitled"）会被 Runtime 自己打破，与前端改得好不好无关；
3. 每次"零 session 重启"都在 SQLite 里累积一条 ownerless 垃圾 session。

其存在理由（注释原话："Without this … the frontend ChatPanel stays blank"）正是本 ADR 要废弃的旧前端契约。必须一并删除（§4.7）。

---

## 3. 目标

1. 前端**不需要任何时序假设、重试计数或超时**就能正确判断"现在能不能读 session"。
2. 删除一个 session 后重启，**不再出现孤儿 session**（含 Runtime 自建的 ownerless session）。
3. 状态语义在后端唯一且权威（消除 §2.5 的注释/实现分裂），且**崩溃后 retained 状态不 stale**（LWT 同主题）。
4. 启动失败对用户**可见**，而不是永远停在"启动中"。
5. "没准备好"与"没有"的区分**贯穿 Runtime → Gateway → Desktop 全链路**（消除 §2.6 的塌缩）。
6. 改动面收敛：不引入第二个布尔补丁，不留兼容分支，不新增主题与订阅。

## 4. 决策

### D1：状态载体与取值——合并进 `AgentStatus`

**不新增主题**。在现有 `acowork/agents/{instance_id}/status`（retained + LWT）的 `AgentStatus` 上扩展：

```protobuf
enum AgentLifecycleState {
  AGENT_LIFECYCLE_STATE_UNSPECIFIED    = 0;  // 未知 / 收到不认识的值 → 门控按 OFFLINE 保守处理，UI 区分展示（§10 Q2）
  AGENT_LIFECYCLE_STATE_OFFLINE        = 1;
  AGENT_LIFECYCLE_STATE_STARTING       = 2;
  AGENT_LIFECYCLE_STATE_HTTP_READY     = 3;
  AGENT_LIFECYCLE_STATE_SESSIONS_READY = 4;
  AGENT_LIFECYCLE_STATE_FAILED         = 5;
}

message AgentStatus {
  string agent_id   = 1;
  bool   online     = 2;
  reserved 3;                      // 曾是 sleeping，永不复用
  string instance_id = 4;          // ADR-073
  string node_id     = 5;          // ADR-073
  AgentLifecycleState state = 6;   // 本 ADR 新增：能力进度
  string detail = 7;               // 本 ADR 新增：仅 FAILED 有值，人类可读失败原因
}
```

合并（而非独立 lifecycle 主题）的理由——**LWT 唯一性是决定性的**：

1. MQTT 每连接**只能注册一个 Last Will**，当前注册在 `status` 主题（`client.rs:986-994`，will payload = `AgentStatus{online=false}`）。合并后，进程崩溃时 will 直接携带 `state=OFFLINE`，retained 状态永不过期失真；独立主题则崩溃后 retained lifecycle **永远停在 `SESSIONS_READY`**，Desktop 对死进程判定"全部允许"。
2. Gateway 的 stop 路径已经是 `status` 主题的第二写者（`agents.rs:1947-1961`，主动发布 `AgentStatus{online=false}` envelope）。合并后 stop → `OFFLINE` 免费获得；独立主题则该路径还要补发 lifecycle，多一个写者、多一处竞态。
3. Desktop 侧 `status` 快照已经由 `agent-event` 通道转发（`workspaceFsEvents.ts` 的 `AgentStatusSnapshot`），合并后 **Desktop 零新增订阅**；独立主题需要新增 `agents/+/lifecycle` 订阅链路。
4. v1 草案反对合并的唯一理由（"LWT payload 带上不需要的字段"）成本是**两个字段、几十字节**，与上述正确性收益不成比例。
5. 编码器复用：`encode_agent_status_payload`（`client.rs:50`）扩展两个参数即可，零新编码器、零新解析、零新测试面。

`online` 保留为**连接层事实**（LWT / stop 驱动），`state` 为**能力层事实**（启动阶段驱动），一致性由不变量 6 约束。

**`ready` 主题直接删除**，不并行发布（不变量 5）。主题路径中的 `{id}` 一律为 **`instance_id`**（ADR-073，与现有 `status` / `ready` 主题一致），实施时不得误用 package id。

### D2：命名原则——按能力，不按阶段

用户提出用 `loading_a / loading_b / loading_... / ready` 这样的阶段命名。**采纳"枚举化"的方向，拒绝"阶段命名"的形式**，理由：

- 状态名会**绑死实现**。Phase A/B/C/D 是今天的启动流程；明天有人合并 B/C、或在 `HTTP_READY` 与 `SESSIONS_READY` 之间插入一个阶段，所有前端的 `case 'loading_b'` 都要跟着改。
- 而前端真正要回答的问题是"**现在能不能调 session 接口**"——这个问题与后端分几步**无关**。
- 能力命名的状态在后端重构后**依然成立**：`loading_b` 一旦失去含义就没有替代品，`HTTP_READY` 则永远有效（HTTP 在听这件事本身就是稳定的语义）。

`STARTING` 是唯一带启动色彩的取值，但它是必要的：它承载"MQTT 刚连、HTTP 还没注册"这个**稳定且长期存在**的窗口（`Gateway 反代 503` 正是这个状态的外显），不是某个可被重构掉的实现细节。

### D3：每个能力边界都发布状态

| 发布时机 | 状态 | 位置 |
|---|---|---|
| **首次** MQTT 连接建立（进程生命周期内仅一次） | `STARTING` | `client.rs` `run_bootstrap`（见 D3b 的重连约束） |
| Phase A 末尾（HTTP 监听 + `http_endpoint` 已发布） | `HTTP_READY` | `agent_init.rs:582`（**替换**现有 `publish_ready(true)`） |
| **Phase B 末尾**（`session_metadata` / `session_config` 槽已填） | `SESSIONS_READY` | `session_init.rs`（**新增**——今天是哑的静默点，正是故障窗口。注意：不再以 initial session 创建为前置条件，见 D7） |
| 任一阶段失败 | `FAILED` + `detail` | 对应阶段 |
| 进程优雅退出 / Gateway stop 路径 | `OFFLINE`（`online=false, state=OFFLINE`） | `publish_status` / `agents.rs:1947` |
| 进程崩溃 | `OFFLINE` | **LWT**（will payload 携带 `state=OFFLINE`，broker 代发） |

`SESSIONS_READY` 的发布时机有一个**必须精确到"之后"的约束**：必须在 `session_init.rs:847`（填 `session_metadata` 槽）**和** `:879`（填 `session_config` 槽）**都完成之后**。否则会重演"宣告了就绪、其实还没好"。

### D3b：重连语义——re-stamp 当前值，禁止回退（v2 新增）

`run_bootstrap` 在**同一进程的重连**上也会跑（ADR-065 sleep/wake 的 `force_reconnect`；Gateway 重启导致内存 broker 丢失全部 retained 状态后的重连）。现有代码已经为此维护了 `ready_ever` 原子标志（`client.rs:524-535`）：Step 7 在重连时 re-stamp retained `ready=true`，而不是重发初始值。

本 ADR 承接并强化该机制：

- `BootstrapData.ready_ever: AtomicBool` → **`current_lifecycle: AtomicU8`**（保存进程内最新状态）；
- `publish_lifecycle(state)` 内置**单调性守卫**：目标状态低于当前值时拒绝发布并 warn（`FAILED` 除外，可从任意非 `OFFLINE` 态进入；进程重启 = 新 `instance_id`，天然重置）；
- `run_bootstrap` Step 7 在重连时随 `publish_status(online=true)` **re-stamp `current_lifecycle` 的当前值**；
- `STARTING` 仅在状态未初始化（首次连接）时发布。

否则：Gateway 重启一次，所有已 `SESSIONS_READY` 的 agent 集体回退到 `STARTING`，Desktop 重新门控 session 接口——违反不变量 3。

### D4：Runtime session 接口的诚实返回

当前 `get_latest_session`（`server.rs:1033`）**不检查 late-bind 槽**，无条件返回 404——这意味着即使 D3 完美实现了，状态传播存在间隙时前端仍会把 404 读成"没有 session"。协议层必须自我保护：

| 条件 | 响应 |
|---|---|
| 槽未填 / 扫描未完成 | `503 {"error":"session_not_ready"}` + `Retry-After: 2` |
| 就绪，有 session | `200` |
| 就绪，确无 session | `404 {"error":"no_session"}` |
| 存在但无权读（ADR-076 私会话） | `404 {"error":"no_session"}`（**body 与上一行完全一致**——刻意不区分，避免向非 owner 泄露"该 agent 有 session"；见 ADR-076 §决策 4） |

`GET /sessions` 槽未填时的裸 503（`server.rs:1015`）同样改为 `{"error":"session_not_ready"}`，让前端无需靠 `Retry-After` 猜测。

**注意区分两处 503 的来源**：Gateway 侧 503（endpoint 未注册，body `{"error":"agent_not_running"}`）与 Runtime 侧 503（body `{"error":"session_not_ready"}`）。两者 body 的 `error` 字段不同，前端可据此判断"该等 `status` 推送还是该重试"。

### D4b：Gateway 反代层状态码透传（v2 新增）

修复 §2.6 的塌缩缺陷，`send_runtime_json`（`proxy.rs:2658-2710`）：

1. endpoint 未注册：`404 "Agent is not running"` → **`503 {"error":"agent_not_running"}` + `Retry-After: 2`**（与 `proxy_to_runtime` 的既有 503 契约对齐，`with503Retry` 可直接复用）；
2. Runtime 返回非 2xx：**透传状态码与 body**，禁止统一映射为 `ApiError::not_found`；
3. 复核 `/api/agents/{id}/conversations/latest`（`chat.rs`）是否仍有消费者——Desktop 主路径走 `/latest-session`（`proxy_latest_session` → `proxy_to_runtime`，本身透传），若确认无消费者则**删除该路由**，减少一条需要维护语义的旁路。

### D5：Desktop 消费——零新增订阅

`status` 快照已经由 Desktop 的 `agent-event` 通道转发（`workspaceFsEvents.ts` 的 `AgentStatusSnapshot`），`state` / `detail` 随 `AgentStatus` 扩展免费到达，**无需新增 MQTT 订阅**（v1 草案的"新增 `agents/+/lifecycle` 订阅"随主题合并而取消）。ADR-058 §3.4 的缺口（`ready` 只能靠 30s 轮询观察）由此消除。

同时删除 6 处 `meta.ready` 消费（`ChatPanel.tsx:1152,1231`、`RightPanel.tsx:381,388`、及 `agentStore.ts:740` 的 `waitForAgentReady`），全部改为按 `state` 判定；`UNSPECIFIED` 与 `OFFLINE` 门控一致（拒绝），但 UI 区分展示（§10 Q2）。

### D6：不写兼容代码

用户已定调"不需要兼容旧版本"。因此：

- `ready` bool 直接删除而非标记 `reserved`；`ready` 主题常量删除；
- `AgentListResponse.ready` / `AgentDetailResponse.ready` / `RunningAgentInfo.ready` 三个字段直接删；
- `AgentStatus` 的 `state` / `detail` 是**新增字段号**（6/7），旧解码器会静默跳过——但 `ready` 主题已删除，旧 Gateway 拿不到任何就绪信号，表现为"agent 一直启动中"而非静默错误数据。Runtime 与 Gateway 必须同版本部署，这是可接受的失败模式。

### D7：删除 Runtime 抢先创建 initial session（v2 新增）

删除 `session_init.rs:966-995` 的 else 分支（无可恢复 session 时 `create_session()` + `set_latest_session` + `set_session_visibility(Private)`）。理由见 §2.7：

1. **单写者**（不变量 7）：session 只能由携带账号身份的 `POST /sessions` 创建，"owned and private from birth"（ADR-076 §决策 4 的原话），不再有 ownerless 垃圾；
2. **零 session 是合法状态**：`/sessions/latest` 诚实返回 `404 no_session`，Desktop 在 `SESSIONS_READY` 后据此创建——这正是决策 10 的触发条件，两条路径由此闭合；
3. 该分支的既有理由（"frontend ChatPanel stays blank"）依赖的是旧前端契约，本 ADR 已废弃之；
4. 不删除它，§8.2 的核心回归断言（"删光 session → 重启 → 0 条 untitled"）**必然失败**，测试无法落地。

有可恢复 session 时的 if 分支（resume + token 合并）**保留不动**——那是恢复既有 session，不是创建新 session。

## 5. 被否决的替代方案

| 方案 | 否决理由 |
|---|---|
| **A. 保留 `ready` + 新增 `sessions_ready`** | 用户明确否决。理由是 §1.2 决策 4：两个 bool 有 4 种组合，其中 `ready=false, sessions_ready=true` **逻辑上非法**。非法状态可表达 = 状态机未定义完整。 |
| **B. `loading_a/loading_b/.../ready` 阶段命名** | 用户提出，本 ADR 采纳方向但否决形式。见 D2。 |
| **C. 只改前端（重试更久 + 不清空列表）** | 治标。协议层仍无法表达"没准备好"，前端永远在猜；且不解决 §2.5 的注释/实现分裂。 |
| **D. 只让 Runtime 返回更准的状态码，不做枚举** | 解决 D4 但不解决启动期（Gateway 503 窗口内 HTTP 不通，前端什么都拿不到）。两者都需要。 |
| **E. Gateway 缓存 `/sessions` 结果以消除窗口** | 用一致性换时序。Gateway 违反 ADR-009 §5 边界（agent 私有数据只经 Runtime HTTP 读写）。 |
| **F. 保留 `ready` 作为派生字段（`state >= HTTP_READY`）** | 用户明确否决（"废弃 ready"）。且派生字段会让"两个概念并存"，正是 A 的问题换个名字。 |
| **G. 独立 `acowork/agents/{instance_id}/lifecycle` 主题（v1 草案倾向）** | v2 否决。LWT 每连接仅一个且挂在 `status`：独立主题下进程崩溃后 retained lifecycle **永久 stale 在 `SESSIONS_READY`**，违反不变量 4；补救手段（Desktop 跨主题优先级规则 / Gateway 代发 lifecycle=offline）都是隐式契约或第三写者，正是 `ready` 出事的根源类型。且 Gateway stop 路径（`agents.rs:1947`）已双写 `status`，合并后 `OFFLINE` 免费获得。详见 D1。 |

## 6. 后果

### 6.1 正面

- 启动窗口内前端有**权威状态**可读，不再靠猜；崩溃后 retained 状态由 LWT 兜底，不 stale。
- "没准备好" vs "没有"在协议层可区分，且经 D4b 贯穿 Gateway 反代层 → §2.4 的三个叠加缺陷与 §2.6 的塌缩缺陷从源头消失。
- session 创建收敛为单写者（D7），ownerless 垃圾 session 不再产生。
- `FAILED` 态让启动失败对用户可见（当前 `degraded_reasons` 只进 `/health`，UI 不看）。
- 消除 §2.5 的注释/实现分裂。
- 零新主题、零新订阅、零新编码器——改动面比 v1 草案更小。

### 6.2 负面 / 成本

- 跨 3 层契约改动，Runtime/Gateway/Desktop 必须同版本。
- Desktop 6 处 `ready` 消费点 + 2 处 session 决策点需要改。
- `AgentStatus` 的发布点从"连接/断开"扩展为"每个能力边界"（约 4 处），需保证单调性守卫与 re-stamp 正确（D3b）。
- `send_runtime_json` 的所有调用方需复核对 404 的既有依赖（改为透传后，部分调用方拿到的状态码会变化）。

### 6.3 边界 / 例外

- `local` 模式：状态机完全一致，Desktop 同样按 `state` 门控。
- **standalone 模式**（`cli.rs:290` 的 `mqtt_client.is_none()` 分支）：无 MQTT 连接、无 Gateway、无 Desktop 消费者，**状态机整体不适用**——该分支不发布任何状态，也不存在"停留在 `HTTP_READY`"的问题（v1 草案 §6.3 对 `cli.rs:340` 的担忧系误读，已关闭，见 §10 Q3）。实施时仅需在代码注释中标注"lifecycle 仅约束 Gateway 模式"。

### 6.4 回滚

`ready` 主题删除使新旧版本无法互通，回滚需要**整体回退**（Runtime + Gateway + Desktop）。考虑到 Desktop 是本机 Tauri 应用、随安装包分发，回滚粒度实际是"回退整个安装包"，可接受。

### 6.5 已知技术债

- `state` 是单一枚举，未来若出现"部分能力降级"（如 session 可用但 workspace 不可用）会需要重新设计（届时可演进为 capability 位掩码，`AgentStatus` 加字段即可，主题与订阅链路不变）。当前阶段（`HTTP_READY` → `SESSIONS_READY`）的能力划分足够。
- `FAILED` 的 `detail` 在进程随后崩溃时会被 LWT 的 `OFFLINE` 覆盖。可接受：崩溃 ≠ 启动失败，日志留痕；Gateway 收到 `FAILED` 时落一条 info 日志作为第二留痕。

## 7. 改动清单

### 7.1 `core/acowork-core`
- `mqtt_proto`：新增 `AgentLifecycleState` enum；`AgentStatus` 增加 `state = 6` / `detail = 7` 字段。
- 删除 `agents/+/ready` 主题相关常量。
- `encode_agent_status_payload` 扩展 `state` / `detail` 参数（LWT payload 填 `online=false, state=OFFLINE`）。

### 7.2 `core/acowork-runtime`
- `mqtt/client.rs`：`publish_ready` → `publish_lifecycle(state, detail)`（内置单调性守卫）；`ready_ever: AtomicBool` → `current_lifecycle: AtomicU8`；`run_bootstrap` 首次连接发布 `STARTING`、Step 7 重连 re-stamp 当前值（D3b）。
- `startup/agent_init.rs:582`：`publish_ready(true)` → `publish_lifecycle(HTTP_READY)`。
- `startup/session_init.rs`：双槽填充完成后**新增** `publish_lifecycle(SESSIONS_READY)`；**删除** `:966-995` else 分支的抢先 initial session 创建（D7，resume 分支保留）。
- `http/server.rs`：`get_latest_session` / `list_sessions` 等按槽状态返回 `503 session_not_ready`；404 body 统一 `{"error":"no_session"}`（D4 表格）。

### 7.3 `core/acowork-gateway`
- `mqtt/dispatch.rs`：删除 `acowork/agents/+/ready` 分支；既有 `acowork/agents/+/status` 分支扩展解析 `state` / `detail`。
- `gateway/state.rs`：`RunningAgentInfo.ready: bool` → `lifecycle: AgentLifecycleState`；删除 `set_agent_ready`。
- `http/agents.rs`：`AgentListResponse.ready` / `AgentDetailResponse.ready` 删除，`lifecycle` 字段加入；stop 路径（`:1947-1961`）的 offline envelope 补 `state=OFFLINE`；收到 `FAILED` 时落 info 日志。
- `http/proxy.rs`：`send_runtime_json` endpoint 未注册改 `503 agent_not_running`；非 2xx 透传状态码与 body（D4b）。
- `http/chat.rs`：复核 `/conversations/latest` 消费者，无则删除路由。

### 7.4 `apps/acowork-desktop`
- **无新增订阅**（`state` 随 `agent-event` 通道的 `AgentStatusSnapshot` 到达）；`AgentStatusSnapshot` 类型扩展 `state` / `detail`。
- `stores/agentStore.ts`：`meta.ready` → `meta.lifecycle`；`waitForAgentReady` 改为等 `SESSIONS_READY`；`fetchLatestSession` 三态化（`ready` / `none` / `unavailable`）；`fetchSessions` 失败不再清空列表。
- `lib/agent-start.ts` + `agentStore.selectAgent`：`createSession` 触发条件收紧为 "`SESSIONS_READY` + 确实零 session"。
- `UNSPECIFIED` 门控按 `OFFLINE` 处理，UI 区分展示"状态未知（版本不匹配？）"。

### 7.5 `dev/ci.sh`
- `run_gateway_fs_redline` 无需改动（本 ADR 不新增 Gateway fs 访问）。
- v1 草案建议的 lint（"`publish_lifecycle` 调用点不得为 0"）**取消**——它防的是错误的事（D3b 说明重连场景下调用点"多"了才危险）。防漏发 `SESSIONS_READY` 由 §8.1 单调性单测 + §8.2 e2e 覆盖。

## 8. 测试策略

### 8.1 单元测试
- **Runtime**：`lifecycle` 单调性——`SESSIONS_READY` 发布后不得回退到 `HTTP_READY`；**重连不回退**——模拟 `run_bootstrap` Step 7 重连，断言 re-stamp 的是当前值而非 `STARTING`（D3b）。
- **Runtime**：槽未填时 `GET /sessions/latest` 返回 `503 session_not_ready`（**不是** 404）；就绪且无 session 时返回 `404 no_session`；无权读时 body 同为 `no_session`。
- **Runtime**：启动路径（零 session 场景）**不产生**任何 `create_session` 调用（D7）。
- **Gateway**：`AgentStatus` 各 `state` 值解析；未知值 → `UNSPECIFIED` → 门控按 `OFFLINE`。
- **Gateway**：`send_runtime_json` 透传——Runtime 返回 503 时调用方拿到 503（不是 404）；endpoint 未注册返回 `503 agent_not_running`（D4b）。

### 8.2 集成测试（e2e）
- **核心回归**（本 ADR 的存在理由）：启动 agent → 在 `SESSIONS_READY` 之前**不得**产生任何 `POST /sessions` 请求。断言 Gateway 日志中 `POST /api/agents/{id}/sessions` 的时间戳晚于 `state=SESSIONS_READY` 的发布时刻。
- 删除全部 session → 重启 → 断言 `message_count=0 AND title IS NULL` 的 session 数量为 0（**依赖 D7**：否则 Runtime 抢先创建的 ownerless session 会使断言自败）→ 断言 Desktop 在 `SESSIONS_READY` 后创建恰好 1 条 owned session。
- `FAILED` 态：注入启动失败 → 断言 Desktop 收到 `FAILED` + `detail`。
- 崩溃态：kill Runtime 进程 → 断言 LWT 后 retained `AgentStatus` 为 `online=false, state=OFFLINE`（不 stale）。

### 8.3 协议兼容性
- 不适用（D6 已定调不同版本部署）。但需在发布说明中标注 Runtime/Gateway/Desktop 必须同版本。

## 9. 实施里程碑

| 阶段 | 内容 | 验证 |
|---|---|---|
| M1 | proto（`AgentStatus` 扩展）+ Runtime `publish_lifecycle`（原子状态 + 单调守卫 + 重连 re-stamp）+ 删除抢先建 session（D7） | 单调性/重连单测、零 session 启动单测 |
| M2 | Runtime session 接口 `503 session_not_ready` + 404 body 统一 | 单测 |
| M3 | Gateway：`status` 分支解析 `state`、删 `ready` 分支、`send_runtime_json` 透传（D4b）、stop 路径补 `state=OFFLINE` | 单测 |
| M4 | Desktop：`AgentStatusSnapshot` 扩展 + 状态驱动决策 | e2e：启动期无 `POST /sessions`；崩溃后状态不 stale |

## 10. 开放问题（v2 已全部决策）

1. **`lifecycle` 是否需要独立于 `status` 主题？——决策：不独立，合并进 `AgentStatus`**（D1，§5 方案 G）。LWT 每连接仅一个且挂在 `status`，独立主题崩溃后 retained 状态永久 stale；Gateway stop 路径已双写 `status`；Desktop `agent-event` 通道已转发 `status` 快照，合并后零新增订阅。v1 草案"倾向保留独立主题"的结论被推翻。
2. **`UNSPECIFIED` 是否等价于 `OFFLINE`？——决策：门控等价，展示区分**。能力门控上 `UNSPECIFIED` 与 `OFFLINE` 一致拒绝（未知状态不得当作可用，符合不变量 1 的保守侧）；UI 上区分渲染——`OFFLINE` 显示"未启动"，`UNSPECIFIED` 显示"状态未知（版本不匹配？）"。D6 同版本部署下出现 `UNSPECIFIED` 的唯一现实原因是部署错误，一个明确提示比"永远启动中"省一次排查。**不做**版本协商/握手协议（YAGNI）。
3. **`cli.rs:340` 绕过 Phase B 的分支是否仍可达？——决策：关闭，无需定义终态**。代码事实：该分支是 `cli.rs:290` `if agent_ctx.mqtt_client.is_some()` 的 else，**仅 standalone 模式可达**（注释原话："this branch only runs when there is no MQTT client"）。standalone 模式无 MQTT 连接、无 Gateway、无 Desktop 消费者，状态机整体不适用，不存在"停留在 `HTTP_READY`"的问题（根本不发布任何状态）。v1 草案 §6.3 的担忧系误读；实施时在代码注释标注"lifecycle 仅约束 Gateway 模式"即可。

## 11. 实施记录（2026-10-01，评审后）

实施 + 代码评审后落地了三个执行层决策，不改变本 ADR 的结论，只记录"怎么做到的"：

1. **D4"扫描未完成 → 503"经由进程级 lifecycle 戳回读实现**。`get_latest_session` 的空 `latest_session` 缓存只有在进程已盖 `SESSIONS_READY` 戳（该戳在后台扫描 seed 完成后才发布，见 D3）之后才回答 definitive 404，否则 `503 session_not_ready`；判定逻辑收敛为纯函数 `empty_cache_is_definitive(cur_lifecycle, has_mqtt_client)`（`http/server.rs`），FAILED 戳同样回答 503（session 子系统从未就绪，404 不诚实）；standalone 无 MQTT 客户端 = 无 lifecycle 权威，保持立即 404（§10 Q3）。Runtime 由此获得协议层自我保护，§7.4 中"createSession 触发条件收紧"由协议保证。
2. **前端不做基于 store `meta.lifecycle` 的二次 createSession 门控**。评审曾建议在 `agentStore.selectAgent` 加 store 侧门控，被否决：MQTT 推送存在延迟，store 字段可能滞后于 Runtime 真实状态，以过期字段拒绝一个协议层已确证的 404 会把用户卡死在"Loading session…"。单一事实源在协议层（第 1 条）。
3. **FAILED detail 在前端闩锁**（`agentStore.lastStartupFailure`）。FAILED→LWT OFFLINE 窗口为毫秒级，进程退出后 Gateway 的 `running_agents` 条目即被移除，REST 无法恢复 detail；`updateAgentLiveness` 收到 `failed` 时事件驱动闩锁，`sessions_ready` 或新一轮 `startAgent` 时清除，`waitForAgentReady` 报错优先读闩锁。§1.2 决策表"FAILED 对用户可见"由此闭合（`state.rs` 的 `lifecycle_detail` REST 字段仅在进程存活期有效，注释已如实标注）。

**测试状态**：§8.1 单测全部落地（新增 `empty_cache_gate_follows_sessions_ready_stamp`、`test_status_unknown_state_degrades_to_unspecified`、`agentStore.startupFailure.test.ts`）；§8.2 e2e 为 follow-up。
