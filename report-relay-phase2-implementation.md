# 云端中继远程访问（设计文档 24）Phase 2 实现报告

**日期**：2026-10-01 ｜ **里程碑**：M0–M6 全部完成 ｜ **设计文档**：[docs/design/zh/24-cloud-relay-remote-access.md](docs/design/zh/24-cloud-relay-remote-access.md)

## 总览

三拓扑模型（① 单机 local / ② LAN 直连 remote / ③ 中继 relay）全部落地。中继是 SNI/Host 路由的**零解析字节管道**——不解析 HTTP、不持有任何账号秘密（仅 pin 设备 Ed25519 公钥）；账号体系唯一在内网 Gateway（登录即达、鉴权唯一裁决点、吊销即时生效）。

| 里程碑 | 内容 | 关键产物 |
|---|---|---|
| M0 ✅ | 协议与依赖 | `acowork-core::relay`（ndjson 控制帧、WS↔字节流适配器、共享 yamux 驱动器、TEARDOWN_GRACE） |
| M1 ✅ | acowork-relay server | core workspace 新成员：SNI/Host 分流、设备域字节管道、axum 控制面（`/tunnel`）、Ed25519 TOFU 注册、单活隧道（GOAWAY 驱逐）、限流、JSON 设备存储 |
| M2 ✅ | Gateway relay-client | `acowork-gateway/src/relay/`（client/identity）：出站 WSS+yamux、挑战-响应、`[relay]` 配置段、enable/disable/status API、断线退避重连 |
| M3 ✅ | 远程 ACL 硬化 | 严格 MQTT listener `:19874`（仅 `user:*:desktop/mobile:*` + 有效 access token + sub 交叉核验）；RemoteOrigin guard `:19877`（debug/fs/配置写 404、node token 403、`/mqtt` WS 桥） |
| M4 ✅ | Desktop 三态模式 | `GatewayMode::Relay`；`MqttTransport{Tcp,Wss}`；relay 模式 WSS + token 凭据（含 soft-restart 刷新）；端点幂等守卫含传输 |
| M5 ✅ | Desktop 远程 UI | 设置页/引导页 relay 选项、`RelayTunnelPanel`（5s 轮询 `GET /api/relay/status`）、relay 模式 fs-browse 降级 |
| M6 ✅ | E2E 验证 | `tests/relay_full_path_e2e.rs` 全链路（本报告 §3），抓出并修复 WS 子协议生产 bug |

## 1. 测试抓出的生产 bug（按发现顺序）

1. **`TungsteniteWriter` 重复投递**（M0，生产级）：`poll_send` 在 flush Pending 后的重 poll 会再次 `start_send` 同一消息——网络背压下消息重复。修法：`in_flight` 状态机（重 poll 只走 flush-only 路径）。
2. **relay 开流丢 tag 字节**（M1）：响应流的首字节 tag 在驱动器交接时被丢。修法：tag 写入并入驱动器 fulfillment。
3. **告别帧不上链路**（M1）：yamux 帧经驱动任务排队，写完立即 abort 驱动 → 帧丢失。修法：`TEARDOWN_GRACE = 150ms`，所有拆除路径先 drain 再 abort。
4. **502 路径 RST**（M1）：未消费请求字节就 close → 内核回 RST。修法：有界 drain 后再 close。
5. **`/mqtt` WS 桥不回显 `mqtt` 子协议**（M6，生产级）：axum 的 `WebSocketUpgrade` 需显式 `.protocols(["mqtt"])` 才会在 101 响应中回显 `Sec-WebSocket-Protocol`。rumqttc 的 Ws/Wss 传输（M4 Desktop relay 模式的生产路径）发子协议并**校验回显**，不回显则握手失败（`SubprotocolHeaderMissing`）。M3 的 e2e 用裸 tungstenite 客户端（无子协议）所以未暴露；M6 测试按 rumqttc 的真实握手形态补上了子协议后立刻失败。修法：`ws.protocols(["mqtt"]).on_upgrade(...)`（`remote_listener.rs`）。

## 2. M4 关键设计决策（Desktop 三态）

- **rumqttc WSS 寻址语义**：`MqttOptions` 的 host 槽在 Ws/Wss 传输下携带**完整 URL**（`eventloop.rs` 用 `split_url` 解析 domain:port 拨号、URL 原样进 WS 握手含 `/mqtt` 路径）。该映射集中在 `acowork-mqtt-session::client::build_mqtt_options`，其余客户端无感知。
- **TLS**：`Transport::wss_with_default_config()` 平台根证书。中继证书须为公共 CA 签发（如 Let's Encrypt）。CryptoProvider 统一（review C1 修复）：rumqttc 0.25 默认 feature `use-rustls` 经 tokio-rustls default 启用 rustls `aws-lc-rs`，与 workspace 的 `ring` 在同批构建 feature 统一后冲突（`ClientConfig::builder()` 无法自动选 provider，运行时 panic）。修法：workspace 与 Desktop 的 rumqttc 均改 `default-features = false` + `use-rustls-no-provider`；`acowork-mqtt-session` 显式依赖 rustls(ring) 并在 WSS 选项构建处 `ensure_crypto_provider()`（`ring::default_provider().install_default()`，幂等，宿主已装则不覆盖）。
- **token 过期 vs 重连**（关键正确性）：access token 15 分钟过期；MQTT 3.1.1 活连接不复鉴权，但**每次重连重发密码**，且严格 listener 拒绝时**不回 CONNACK 直接断连**（rumqttd `InvalidAuth`）——不刷新凭据则过期后重连进入无限静默循环。修法：`MqttCredentials.refresher` 接 `MqttClientHandler::on_soft_restart`（与 Node 换 node_token 同款机制）。
- **client_id 账户段**：relay 模式从 token payload 本地解 `sub`（base64url，**不验签**——broker 验签并交叉核验 `sub == client_id name`，本地误读只可能失败不可能越权）。
- **端点身份含传输**：`AppState::mqtt_endpoint` 从 `(String, u16)` 升级为 `MqttEndpoint{Tcp, Wss}`——同地址的 TCP 与 WSS 是不同端点，模式切换必然重建客户端。

## 3. M6 全链路验证（`tests/relay_full_path_e2e.rs`）

拓扑（全部真实组件、零 echo 替身）：

```
reqwest / tungstenite 客户端
  → acowork-relay（plain 模式，Host 路由）
    → 设备域字节管道（yamux 隧道）
      → Gateway relay client 流解复用
        → 真实 remote HTTP listener
          ├─ RemoteOrigin guard（§7.2 ACL）
          └─ /mqtt WS 桥（含 mqtt 子协议回显）
              → 真实严格 MQTT listener → 共享 broker（Ed25519 token 鉴权）
```

断言：`GET /health` 经隧道 200；`/api/debug/*`、`/api/fs/browse` 经隧道 404（ACL 端到端生效）；有效 token 的 MQTT CONNECT 经隧道 CONNACK rc=0；错误密码无成功 CONNACK；`disable` 后设备域 502（DEVICE_OFFLINE）。

## 4. 遗留与后续（非 Phase 2 范围）

- **Node 反代 token 鉴权 + enrollment token 签发**（ADR-055 Phase 5a，原 ⑥）。
- **F7 同机命令降级**：M5 已做 fs-browse 按钮降级；剪贴板/附件的完整降级清单（§8.1）待产品确认后落地（LAN 模式同样适用）。
- **TLS 生产部署**：中继需公共 CA 证书（当前测试用 plain 模式 Host 路由验证同一路径）；Desktop 侧 reqwest/rumqttc 已按公网 CA 假设实现。
- **Mobile App**（§8.3）：协议面与 Desktop relay 模式完全一致，待独立排期。
- CLI `package` 命令仍 stub；agent→node 归属未在 UI 暴露（`AgentListResponse` 无 `node_id`）——ADR-055 遗留项。

## 5. 测试快照（2026-10-01，review 修复后复测）

- `acowork-core` 全量：243 passed（含 relay 协议/驱动器/WS 适配器 11 测）
- `acowork-relay`：lib 6 + `tunnel_e2e` 10 passed（握手/转发/单活驱逐/TOFU pinning/TOFU 关闭/502）
- `acowork-gateway` lib relay（policy/identity/M3 e2e）：9 passed ｜ `relay_client_e2e`：2 passed ｜ `relay_full_path_e2e`（M6）：1 passed
- `acowork-mqtt-session`：63 passed（含 WSS 寻址 2 新测）
- **C1 回归场景**：`cargo test -p acowork-relay -p acowork-core -p acowork-mqtt-session` 同批构建全绿（修复前该组合下 `wss_config_builds_url_addressed_options` 因 CryptoProvider feature 统一冲突 panic）
- `acowork-desktop`（Rust）：35 passed / 4 ignored（含端点守卫、WSS URL 推导、token sub 解析 5 新测）※本轮未复跑，快照沿用
- 前端 vitest：settingsStore×4 + SplashScreen×2 + GatewayBanner 共 29 passed；`tsc --noEmit` 干净；i18n 5 locale 检查通过
- clippy `-D warnings`：acowork-core / relay / gateway / mqtt-session 全绿（review 修复后复测）；desktop `cargo check` 通过；`acowork-runtime`/`acowork-node` 编译检查通过

## 6. Review 修复记录（2026-10-01，代码评审后）

| # | 级别 | 问题 | 修复 |
|---|---|---|---|
| C1 | Critical | rumqttc 默认 feature 引入 rustls `aws-lc-rs`，与 workspace `ring` feature 统一冲突 → Desktop relay 模式 MQTT WSS 生产路径（`build_mqtt_options` → `wss_with_default_config`）运行时 panic | workspace + Desktop 两处 rumqttc 改 `default-features=false` + `use-rustls-no-provider`；mqtt-session 增 rustls(ring) 依赖 + `ensure_crypto_provider()` 显式装 ring（幂等）；cargo tree 验证 gateway/mqtt-session/desktop 三图 `aws-lc-rs` 清零 |
| M1 | Major | `relay_identity.json` 私钥先 rename 后 chmod，存在 0644 窗口 | 改为 tmp 文件先 `set_permissions(0600)` 再 rename（原子发布语义完整） |
| m1 | Minor | `proto.rs` 注释仍写 “vault-backed”，与 v0.2.1 决策矛盾 | 注释修正为 0600 文件 + 理由 |
| m2 | Minor | `RelayClient::disable()` 不等待 supervisor 退出，disable→enable 可短暂双隧道 | 同步等待（5s 超时兜底 abort），Deregister+grace 完成后 API 才返回 |
| m3 | Minor | `peek_request_host` 每轮把未消费的 peek 数据重复追加进 buf | 按窗口增量只追加 delta，buf 保持请求头真实前缀 |
| m4 | Minor | relay `main.rs` 空 if 死代码 | 删除，保留说明注释 |
| m5 | Minor | `clipboard/macos.rs` 无关重构混入本分支 | 已回退（`git checkout`），保持分支为独立模块交付 |
