# ADR-089: LSP Relay 可达性与暴露面的收敛——Desktop 经 Gateway 反代 relay

**状态**：提议（待架构评审）
**日期**：2026-10-09
**决策者**：待评审（用户定案方向：先修可达性，再以反代收回暴露面）

**关联**：
- [ADR-055](./ADR-055-remote-runtime-node-topology.md)（§6.3 endpoint/advertise 模型 D3、§6.4 Runtime 访问链路 D2 两跳反代、§6.7 Sidecar Scope 模型、§6.8 安全模型、§6.3.3 动态地址自愈）
- [ADR-019](./ADR-019-lsp-relay-standalone-process.md)（LSP Relay 独立进程）
- [ADR-030](./ADR-030-sidecar-endpoint-dynamic-push.md)（Sidecar 端点动态推送）
- [ADR-080](./ADR-080-gateway-advertise-host-ip-change-watchdog.md)（advertise_host 漂移自愈）
- [ADR-087](./ADR-087-node-agent-owner-permissions.md)（Node / Agent 归属与权限）
- [ADR-076](./ADR-076-multi-user-account-system.md) / [ADR-084](./ADR-084-user-standalone-process.md)（Desktop 侧凭据形态）

---

## 1. 决策摘要

### 1.1 一句话

**LSP relay 的唯一消费者是 Desktop 浏览器，而 relay 进程自身零鉴权——所以它不该、也不能直接暴露给 LAN。**
终局形态：Desktop 访问 relay 一律走 **Gateway 反代**（与 §6.4 访问 Runtime 同构的两跳链路），relay 绑定收回 `127.0.0.1`；
`acowork/nodes/{id}/lsps` retained topic 只作健康信号，**其通告的 URL 不再被任何消费方当作可直连地址**。

### 1.2 关键决策表

| # | 决策 | 内容 |
|---|------|------|
| D1 | relay 绑定收回 loopback | relay `--host` 恒为 `127.0.0.1`（撤销临时修复的「bind 跟随 advertise」默认行为），仅保留显式逃生阀 `expose_lsp_relay` |
| D2 | Gateway 新增 relay 反代路由 | `/{...}/api/nodes/{node_id}/lsp/*` → `http://{node.proxy_endpoint}/...`，注入 `X-ACowork-Node-Token`，复用 `http/proxy.rs` 既有转发机制（含 WebSocket 升级透传） |
| D3 | endpoint 组装改代理 URL | `GET /api/agents/{id}/lsp-endpoint` 返回 `{gateway_advertise}/api/nodes/{node_id}/lsp`，不再返回 node 的 LAN 地址；topic 值语义不变（避免 Node 侧改动、兼容旧节点） |
| D4 | online 门控 | `get_agent_lsp_endpoint` 必须校验 `n.online`；节点掉线返回 `ready:false` + 原因码，而非返回死地址 |
| D5 | bind 与 advertise 同源 | Node 反代的 bind 地址改为跟随 §6.3.3 `live_advertise_host` 快照（IP 漂移时重绑），删除 `start` 缺省路径上的一次性 LAN 探测 |
| D6 | codebase 工具接入 relay | **移出本 ADR 范围**（见 §2.4），另案处理 |

---

## 2. 背景与问题

### 2.1 现场事实（2026-10 排障）

拓扑：Gateway + Desktop 在 `192.168.5.82`；Node（`node_id=3e314b69`）+ Desktop 在 `192.168.17.113`。
现象：harness 面板点击 LSP，刷新不出 server 列表，浏览器报 `Failed to fetch`。

实测不对称：`192.168.17.113:19900`（Node 反代，bind `0.0.0.0`）返回 200；`192.168.17.113:19878`（relay）超时 / CLOSED-FILTERED。

### 2.2 根因链

1. **bind ≠ advertise（违反 §6.3 D3）**：`acowork-node/src/sidecar/lsp_relay.rs` 曾把 relay 的 `--host` 写死 `127.0.0.1`，而 `lsps` topic 通告 `http://{advertise_host}:19878`。浏览器按通告值直连 → 连到本机 → 失败。
2. **掉线节点仍返回死地址**：`acowork-gateway/src/http/agents.rs:1088` 的 `get_agent_lsp_endpoint` 只读 `n.lsp_endpoint`，**不看 `n.online`**，且 `ready = endpoint.is_some()`。节点掉线后 retained 值仍在，面板拿到一个永远连不通的地址，前端只能显示成网络错误。
3. **bind 与 advertise 分叉的结构性来源**：`acowork-node/src/cli.rs` 的 `start` 在未显式传 `--addr` 时走一次性 `detect_non_loopback_ipv4()` 探测；而 advertise_host 由 §6.3.3 的 if-watch 快照持续刷新。两者不同源，换网段后必然分叉。（对照：Gateway 自管 spawn 在 `gateway/node_manager.rs:563` 显式传 `--addr 127.0.0.1:{port}`，走的是另一条分支。）

### 2.3 临时修复与其代价

第一步修复（relay bind 跟随 `cfg.advertise_host`）让面板立即可用，但代价是把**一个完全没有鉴权的进程暴露到 LAN**：relay 暴露 `POST /api/lsp/install/{language}`（执行安装脚本）、`GET /api/lsp/servers-with-status`、以及 `/lsp/{language}` 的 WebSocket（spawn LSP server、读写 workspace 文件）。LAN 上任意主机可直连利用。

### 2.4 附带发现：§6.7 的一半尚未落地

§6.7 声明 relay 的 endpoint 分发给「Runtime（codebase 工具）与 Desktop（Monaco）」，但 `acowork-runtime/src/.../agent_init.rs:892` 写死 `let lsp_relay_endpoint: Option<String> = None;`（ADR-040 分层遗留）。**CodebaseTool 从未注册，relay 至今的唯一消费者是 Desktop。** 这不影响本 ADR 的结论（浏览器无法注入自定义头，方案 B 无论如何不成立），但意味着「Runtime 跨节点调用 relay」是一条未验证的假设路径，需另案。

---

## 3. 备选方案

| 方案 | 描述 | 结论 |
|------|------|------|
| A | relay bind 跟随 advertise，直接暴露 LAN（= 第一步现状） | 可达，但无鉴权进程暴露到共享网络。否决为终局，仅留作显式逃生阀 |
| B | relay 自身校验 node token | **不可行**：消费方是浏览器，`fetch` / `WebSocket` 无法注入自定义请求头；Cookie 受同源约束跨源不可用；token 只能落进 URL 或前端存储，等于把长期凭据下发到任意 Desktop |
| C | Desktop 经 Gateway 反代 relay | **选定** |
| D | relay 迁回 Gateway 机器（恢复 ADR-019 单机形态） | 违反 §6.7 的技术依据：`acowork-lsp-relay/src/codebase.rs` 的 `root_uri = file://{workspace_root}` 要求 LSP server 与 workspace 同机。否决 |

---

## 4. 决策理由

1. **与既有链路同构，不新增协议面。** Gateway 已在 `http/proxy.rs:1170` / `:2595` 对 Node 反代注入 `X-ACowork-Node-Token`，Node 已在 `proxy/mod.rs:201` 校验。relay 反代只是多一条上游路由，不是第三套鉴权体系。
2. **收回暴露面即消除 §6.7 与 §6.8 的冲突。** §6.7 隐含「消费方按通告值直连」，§6.8 的安全模型只覆盖 MQTT CONNECT 鉴权、Node 反代入站校验、Gateway 对端 IP 白名单——relay 不在任何一处。让 relay 不对外，两条条款自动自洽。
3. **顺带解掉多节点歧义。** 用户疑问「Gateway 连多个远程 node，每个都有 LSP，面板刷新显示哪一个」：URL 恒为 Gateway 侧，node 由 agent → node 归属解析（§6.7 的 `GET /api/agents/{id}/lsp-endpoint`），面板不再需要理解 node 的 LAN 地址；跨 node 对比才需要面板级切换。

---

## 5. 详细设计

### 5.1 Gateway 反代路由

```
GET/POST/DELETE  /api/nodes/{node_id}/lsp/*
  → 校验 Desktop 侧凭据（现有 Gateway HTTP 鉴权中间件，不变）
  → 按 ADR-087 校验调用者对 node_id 的可见性
  → node_registry[node_id] 取 proxy_endpoint；!online → 503 + 原因码
  → 注入 X-ACowork-Node-Token，转发至 http://{proxy_endpoint}/{...}
```

WebSocket：`/lsp/{language}` 需要 `Upgrade` / `Connection` 透传。**现状是反的**：`http/proxy.rs:2457` / `:2464` 已把 `connection`、`upgrade` 归入 hop-by-hop 过滤表（`is_hop_by_hop_header`，测试见 `:3177` / `:3184`），即当前反代会静默剥掉升级头、把请求降级为普通 GET。P1 必须为 WS 路由开显式豁免通道并转发 `Sec-WebSocket-*`。这是本方案唯一的技术风险点，也是 P1 的第一条测试。

### 5.2 endpoint 组装（D3）

`lsps` retained topic 的载荷格式**不变**（仍是 node 侧 `http://{advertise_host}:19878`），Gateway 在 `get_agent_lsp_endpoint` 里丢弃该值的路由含义、改组装代理 URL。理由：topic 是 node→Gateway 的健康上报，改语义要同时动 Node 与所有已部署旧节点；代理 URL 完全可由 `node_id` + Gateway `advertise_host` 推出，无需通告。

topic 值的唯一保留用途：Node 侧自证 relay 已就绪（`ready` 的输入之一）。

### 5.3 online 门控与原因码（D4）

```rust
// 现状：let ready = endpoint.is_some();
// 终局：节点离线时不得返回可连接地址
if !n.online { return AgentLspEndpointResponse { endpoint: None, ready: false, reason: Some("node_offline") } }
```

原因码枚举最小集：`node_offline` / `relay_not_ready` / `no_lsp_sidecar`。前端据此显示可读文案，而非 `Failed to fetch`。

### 5.4 bind 与 advertise 同源（D5）

Node 反代**必须**对 Gateway 可达（跨机部署的前提），所以 D5 不是「把 `--addr` 默认改成 loopback」——那会直接打断远程节点。正解是让 bind 跟随 §6.3.3 的 `live_advertise_host` 快照，IP 漂移时重新绑定，与通告值恒等；一次性探测分支删除。

### 5.5 逃生阀（D1）

`[node].expose_lsp_relay = false`（默认 false）。置 true 时 relay bind 跟随 advertise——仅供完全隔离的实验网络，文档须标注为不受支持形态。

---

## 6. 后果

### 6.1 正面

- relay 回到 loopback，LAN 上无可达面；安装脚本执行、LSP spawn、workspace 读写全部经 Gateway 鉴权与归属校验。
- bind/advertise 分叉被结构性消除（D5），换网段不再产生「一个能连、一个不能连」的不对称。
- 面板不再依赖 node 的 LAN 地址，多节点场景语义清晰。
- 掉线节点给出可读错误而非网络超时。

### 6.2 负面与代价

- 两跳反代引入额外延迟（编辑器 LSP 请求每跳一次；本机同进程转发，量级为本地回环，可接受）。
- Gateway 需支持 WebSocket 透传（§5.1 风险点）。
- 远程节点上 Gateway → Node 的 LSP 流量走 Node 反代端口（19900），与 Runtime 流量共享带宽与连接数上限。
- 第一步已合入的 bind 修复在 P3 被收回，属可接受的回退（同一 PR 序列内自洽）。

### 6.3 风险

| 风险 | 缓解 |
|------|------|
| WS 升级被 hop-by-hop 过滤吃掉，表现为「HTTP 接口全通、编辑器不亮」 | P1 先写透传测试；实测用 Monaco 连一个真实 server |
| rumqttd 0.20 无 topic 级 ACL（§6.8 已记录偏差），任意已注册节点可发伪造 `lsps` | D3 下 Gateway 不信 topic 值、只信 registry 的 `node_id` 归属，天然免疫 |
| 现有依赖直连 relay 的第三方/脚本被破坏 | relay 从未有稳定对外契约（endpoint 随 IP 漂移），不算破坏既有接口；CHANGELOG 标注 |

---

## 7. 实施边界

| Phase | 内容 | 验收 |
|-------|------|------|
| P1 | Gateway relay 反代路由 + WS 透传 + node token 注入 | 反代路由单测；WS 升级透传测试；离线节点 503 |
| P2 | `get_agent_lsp_endpoint` 改代理 URL + online 门控 + 原因码 | 多节点场景返回正确 node 的代理 URL；掉线返回 `ready:false` |
| P3 | Desktop 切代理 URL；relay bind 收回 `127.0.0.1`；`expose_lsp_relay` 逃生阀 | 跨机实测 harness 面板 + Monaco 编辑器均可用 |
| P4 | D5：Node 反代 bind 跟随 `live_advertise_host` | 换网段后无需重启即恢复可达（if-watch 触发重绑） |

**不在本 ADR 范围**：Runtime codebase 工具接入 relay（§2.4，另案）；Phase 5b 公网档（TLS / payload 加密）；topic 级 ACL（依赖 broker 替换评估）。

---

## 8. 回滚策略

P1/P2 是纯增量路由与组装逻辑，回滚 = 恢复返回 topic 通告值 + relay bind 跟随 advertise（即第一步现状，功能可用但暴露面重新打开）。回滚开关即 §5.5 的 `expose_lsp_relay`，无需代码回退。

---

## 9. 开放问题

1. LSP 反代路由的归属校验粒度：按 agent 还是按 node？（ADR-087 定稿前，P1 沿用现有 node 可见性门控）
2. 面板是否需要「跨 node 对比」视图——当前语义是「agent 所在 node」，多节点全览是产品决策，不是协议决策。
3. Runtime codebase 工具接入后，链路变成 Runtime → Gateway → Node → relay 三跳，是否允许 Runtime 直连 Node 反代（携带 node token 由谁签发）——另案 ADR。
