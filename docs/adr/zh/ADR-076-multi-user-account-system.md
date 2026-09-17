# ADR-076: 多用户账号系统

**状态**：草案（待评审）
**日期**：2026-10-15
**决策者**：大鱼

**前置**：
- [ADR-009](./ADR-009-gateway-workspace-isolation.md)（§5.4 Gateway 边界规则 — 用户聊天数据归属 Gateway 自身，不在该边界禁止范围内，但要在本文档显式落字）
- [ADR-024](./ADR-024-merge-metadata-into-index.md)（conversation 持久化范式 — meta.json + jsonl 双文件 — 是用户聊天的复用模板）
- [ADR-055](./ADR-055-remote-runtime-node-topology.md)（Node 拓扑 — Node 拥有 `install_path`，session 数据物理上落在 Node；本 ADR 不打破这点，但 session 过滤维度从 agent_id 扩到 `(instance_id, user_id)`）
- [ADR-073](./ADR-073-agent-instance-identity-decomposition.md)（agent instance / node / user 三层身份范式 — 本文把 user 提升为与 instance、node 同级别的身份维度）
- [ADR-059](./ADR-059-parallel-onboarding-handshake.md)（Vault Argon2id + ChaCha20-Poly1305 KDF 链 — 本 ADR 复用同一 vault 的 master key 派生 per-user 加密条目，而非新建密码体系）

---

## 1. 决策摘要

### 1.1 一句话

**把"用户"从纯展示偏好提升为一等身份维度**：`UserProfile` 升级为 `UserAccount`（带账号/密码/角色/admin flag），账号凭据通过现有 Vault 的 master key 加密落盘；session 元数据增加 `user_id` 字段，Runtime `GET /sessions` 接受 `?user_id=` 过滤参数实现 session 级隔离；新增"系统管理员"角色绕过所有 session 隔离；Agent 列表侧栏并排新增"User List"折叠分组（复用 `partitionAgentsByNode` 的分组范式）；Gateway 侧新增用户-用户聊天持久化（conversion.json / jsonl 双文件，参考 ADR-024 拆分）。

### 1.2 关键决策表（详细理由见 §4）

| # | 决策 | 结论 |
|---|---|---|
| 1 | 账号数据模型 | 升级 `UserProfile` → `UserAccount`，新增 `password_hash`(Argon2id)、`password_salt`、`role`(`user`/`admin`)、`created_at`、`disabled_at?`；display_name / language / avatar 等展示字段保留 |
| 2 | 凭据存储 | **复用现有 Vault 的 master key**，account 文件以 `vault://accounts/{user_id}.enc` 形式加密落盘；账号创建/改密均要求 Vault unlocked；**不引入第二套密码** |
| 3 | HTTP 认证 | 单一 bearer token 改为 **登录令牌**（短期 access_token + 长效 refresh_token），token payload 含 `user_id` + `role`；middleware 解析后注入 `AuthContext` 到 `AppState`；admin token 通过额外 flag 区分 |
| 4 | Session 隔离 | `SessionMeta` 新增 `user_id: Option<String>` 字段；Runtime `/sessions` 接受 `?user_id=` 过滤；admin 看到全部，普通 user 只看到自己的（admin 时强制忽略过滤） |
| 5 | 管理员角色 | `role = "admin"` 用户绕过 `user_id` 过滤，且 `GET /api/users` 看到全部账号（含密码哈希元数据但不含明文）；普通用户只能 `GET /api/users/{self}` |
| 6 | Desktop 账号切换 | 顶栏新增"当前用户"菜单，下拉含"切换账号 / 修改密码 / 注销 / 退出登录 / 注册新账号(若允许)";账号切换等价于"清空本地缓存 + 重连 Gateway + 重新拉取 agent列表 + 重连 MQTT" |
| 7 | Sidebar User 折叠分组 | 在 AgentList 同级渲染 `partitionAccountsByAccountType` 折叠项——单独 item "Users (N)" 默认折叠，点击展开列出全部账号；admin 视图下点击账号名进入该 user 的 session 列表过滤模式 |
| 8 | 用户-用户聊天 | Gateway 侧新增 `data_dir/users/{user_a_id}/chats/{user_b_id}/conversation.json` + 同名 `.jsonl`（按字典序排 `(min(a,b), max(a,b))` 避免重复）；参考 ADR-024 meta/jsonl 拆分；不支持 group chat |
| 9 | 存储归属 | 用户聊天数据完全归属 Gateway 所在机器（`data_dir/users/...`），**不**走 Runtime HTTP 反代；明确写入 ADR-009 §5.4 例外条款 |
| 10 | 反代身份注入（PM/Doc） | REST 反代 `X-Actor` 从硬编码 `"human"` 改为 `AuthContext.effective_user_id`（决策 3 的 token 身份）；MCP 路径 `X-MCP-Actor` 校验不变（agent 身份与 user 正交，见 §决策 10） |
| 11 | PM 成员模型多用户化 | `ProjectMember` 新增 `kind`（`Agent`/`User`），人类操作者与 agent 成员对称；移除 `assignee = "human"` 特例，不变式收紧为 `assignee ∈ ∅ ∪ members`（见 §决策 11） |
| 12 | 部署模式分流 | 新增 `AUTH_MODE ∈ {local, multi_user}`，由 bind 地址自动推断（`127.0.0.1` → `local`；`0.0.0.0` → `multi_user`），可显式覆盖；local 模式下 §1-§11 退化为 no-op（不引入登录页 / Argon2id / admin / session 过滤 / 用户聊天），multi_user 模式下完整启用且强制 `bootstrap_admin`（缺则拒启动） |

### 1.3 不变量（必须满足）

1. **multi_user 模式下 session 隔离是强制默认**：除 admin 外，所有 session 维度的读写（list / messages / state / files）必须经过 user_id 过滤；漏掉任何一处 = 数据泄漏。local 模式下不启用 session 过滤（见 §决策 12）。
2. **admin 不能伪造 user_id**：admin 视图下"以 user A 身份看 session"通过 `?as_user=<user_id>` query 实现，但 `as_user` 不会被普通 user 使用；token 中 `role` 字段在签发时定死，不接受请求内覆盖。
3. **账号凭据加密不依赖 Vault unlocked**：账号读路径在 Vault locked 状态下退化为 401（无法解密 → 无法登录），但**账号列表（不含密码）的元数据允许在 Vault locked 时展示**（仅元数据，如 username/role/created_at），便于锁屏场景下仍能选账号。
4. **session.user_id 写入是 immutable**：一个 session 一旦创建绑定 user_id 后**不再修改**（迁移/导入等场景除外，且必须 admin 操作）；这保证会话历史"主人"的不可篡改性。
5. **聊天双方对等**：用户 A → 用户 B 的消息存在 `min(a,b)/chats/max(a,b)/` 目录下，双方 GET / POST 对称，无需在 Gateway 内维护 per-user 状态机。
6. **密码修改强制旧密码**：改密 API 接受 `old_password + new_password`，避免 token 泄漏后任意改密；admin 不能改他人密码（必须先 reset 再走首次登录改密流程）。

### 1.4 部署模式行为对照表（§决策 12 速查）

| 维度 | `AUTH_MODE=local`（bind `127.0.0.1`，默认） | `AUTH_MODE=multi_user`（bind `0.0.0.0`） |
|---|---|---|
| HTTP 认证 | 现有 bearer token（`data_dir/http_token`） | access_token(HS256, 15min) + refresh_token(30d) |
| 登录流程 | 无（Desktop 直接用 token） | `/api/auth/login` + LoginView |
| `UserAccount` schema | `user_profiles.json` 沿用现有 `UserProfile` 字段 | `UserAccount`（含 `password_hash` / `role` / `disabled_at`） |
| `accounts.json`（账号权威表） | 不创建 | 创建（明文）；`vault/accounts/*.enc` 扩展可选 |
| 首位账号 | 沿用现状（无账号概念，`user_profiles.json` 不变） | 强制 `bootstrap_admin`，**缺配拒启动** |
| admin 角色 | 无（OS 用户即 admin） | `role = Admin`，`GET /api/users` 全量 |
| session 隔离 | `SessionMeta.user_id` 写入但 **read 不过滤** | read 路径强制 `user_id` 过滤（admin 除外） |
| `?as_user=` | 路由不注册 | admin-only 只读视图 |
| `/api/auth/*` 路由 | **不注册** | 全部注册 |
| 用户-用户聊天 | **不注册** `data_dir/users/` 不创建 | `/api/users/{self}/chats/*` 全量 |
| PM/Doc 反代 `X-Actor` | 常量 `"human"` | `auth.effective_user_id` |
| PM `assignee == "human"` 特例 | **保留**（无 token 身份可注入） | 移除，`assignee ∈ ∅ ∪ members` |
| Desktop 顶栏 | "用户偏好"（现状不变） | "账号菜单"（切换 / 改密 / 注销 / 退出） |
| Desktop 侧栏 User 分组 | 不显示 | `partitionAccounts` 折叠分组 |
| 升级路径 | → multi_user：补 `password_hash` + `invite_token` 激活（无需数据迁移） | — |
| 降级路径 | — | → local：`--auth-mode local`，账号文件保留但不可登录 |

---

## 2. 背景与动机

### 2.1 现状：一个用户，一份配置，全局共享

```text
                        ┌────────────────────────┐
                        │       Desktop App      │
                        │   (单一 localStorage)  │
                        └──────────┬─────────────┘
                                   │ HTTP + Bearer Token (单一共享)
                                   ▼
                        ┌────────────────────────┐
                        │       Gateway          │
                        │  HttpAuth (1 token)    │
                        │  user_profiles.json    │  ← 所有 user 平铺，无认证
                        └──────────┬─────────────┘
                                   │ MQTT
                                   ▼
                        ┌────────────────────────┐
                        │   Runtime (per inst)   │
                        │  conversations/meta/   │  ← 无 user_id 字段
                        │  conversations/*.jsonl │
                        └────────────────────────┘
```

**关键缺陷**：
- `UserProfile` 是"显示偏好"，不是"账号"——任何前端都能 `POST /api/users` 注册新 user，也能在 `is_active=true` 时把自己的 profile 推到 Runtime `last_user_profile`。
- `HttpAuth` 的 bearer token 是 Gateway 启动时随机生成的 32-byte hex，所有 Desktop 实例**共享同一个 token**；`http_token` 文件一发则所有机器同权。换言之：登录的是"这台 Desktop"，不是"这个用户"。
- `SessionMeta` 不携带 user_id，`GET /api/agents/{id}/sessions` 返回**所有**会话；Desktop 上"我创建的会话 vs 别人创建的"无法区分。
- 没有"管理员"概念——所有 user 平权，谁也看不到全局。

### 2.2 已有可复用的积木

| 积木 | 现状 | 本文复用点 |
|---|---|---|
| `Vault`（Argon2id + ChaCha20-Poly1305）| [core/acowork-vault/src/vault.rs](core/acowork-vault/src/vault.rs) 一个 master key 派生所有 `.enc` 条目 | 用户账号文件以 `vault://accounts/{user_id}.enc` 复用同一 master key |
| `partitionAgentsByNode`（Node 折叠范式）| [apps/acowork-desktop/src/components/agent-list/partitionAgentsByNode.ts](apps/acowork-desktop/src/components/agent-list/partitionAgentsByNode.ts) | 新增 `partitionAccounts` 完全照搬——单一折叠分组 vs 多 node 分组是同构问题 |
| `SessionMeta` + jsonl（ADR-024）| meta.json 400 bytes + jsonl 流式 append | 用户聊天 = `conversation.json` (meta) + `conversation.jsonl` (流)；schema 字段不同 |
| `HttpAuth::validate_token`（常量时间比较）| [core/acowork-gateway/src/http/auth.rs:60](core/acowork-gateway/src/http/auth.rs#L60) | token 改为签名的 JWT-ish，验证代码替换为 signature 检查 |
| `OperationAck` + `expected_version`（ADR-059 §7.3）| [core/acowork-gateway/src/http/users_api.rs:147](core/acowork-gateway/src/http/users_api.rs#L147) 乐观并发 | 账号创建/改密/角色变更都走同一乐观并发协议 |

### 2.3 已尝试 / 已拒绝的方案（避免重蹈覆辙）

- **新建第二套密码系统（与 Vault 解耦）**：❌ 用户被迫记两个密码；salt/迭代参数分裂两份；运维成本翻倍。
- **账号信息塞进 `UserProfileListFile`**（现有 json）：❌ 该文件路径 `data_dir/user_profiles.json` 当前明文��盘，把账号凭据混进来会破坏 ADR-059 §7.3 的"非敏感展示元数据"语义。
- **让 Runtime 内置 user store**：❌ Runtime 是无主进程，跨 Node 上同一个 agent instance 的 user 视图无法合并；且 ADR-009 §5.4 禁止 Gateway 直读 Runtime 私有数据，方向反了。
- **session 过滤在 Gateway 侧用 `instance_id` 索引文件实现**：❌ 引入第二索引源（与 Runtime scan_sessions 并存），维护两套真相；session 持久化路径全在 Runtime，重复造轮子。

---

## 3. 目标

1. **账号层**：注册 / 登录 / 改密 / 注销账号全流程；密码 Argon2id 哈希 + Vault 加密存储；不引入第二套密码体系。
2. **session 隔离**：每个 session 持久化时绑定创建者 user_id；普通 user `GET /api/agents/{id}/sessions` 默认只看自己；admin 看全部。
3. **管理员**：内置 admin 角色；安装 Gateway 时通过环境变量 / 首次启动交互创建首位 admin；admin 可看全部数据但不能伪造 user_id 操作（"以 user X 身份查看"通过 `?as_user=` query 而非身份冒用）。
4. **Desktop UI**：顶栏账号切换 / 修改密码 / 注销 / 注册入口；侧栏 Agent 列表旁增加 User 折叠分组（admin 视图下点击账号可进入"以该 user 视角看 session"模式）。
5. **用户-用户聊天**：文字 + 图片 + 文档；conversion.json (meta) + conversion.jsonl (流) 持久化在 Gateway 本地 `data_dir/users/{a}/chats/{b}/`；不走 Runtime HTTP。
6. **存储**：账号 + 用户聊天全部位于 Gateway 所在机器（`data_dir/` 下），不依赖 Node agent 文件系统；session.user_id 维度不改变 Runtime 物理数据布局（仍按 `{install_path}/workspace/conversations/` 落地）。

---

## 4. 决策

### 决策 1：账号数据模型 — `UserProfile` → `UserAccount`（in-place 升级，迁移脚本同表转换）

```rust
// core/acowork-core/src/account.rs (新文件)
pub struct UserAccount {
    // ── ADR-076: 身份字段 ──
    pub user_id: String,                  // UUID v4，复用现有 user_id 语义
    pub username: String,                 // 新增：登录用唯一 handle（小写 + 数字 + -_）
    pub display_name: String,             // 旧 UserProfile.display_name
    pub role: Role,                       // User / Admin

    // ── ADR-076: 凭据字段 ──
    /// Argon2id PHC string: "$argon2id$v=19$m=...,t=...,p=...$<salt>$<hash>"
    /// 单独存 —— 独立于 Vault —— 目的是让登录校验在 Vault locked 时也能完成
    /// （Vault 是用来加密敏感扩展字段，不是密码哈希本身）
    pub password_hash: String,
    pub password_changed_at: String,      // ISO8601，强制改密策略用
    pub password_expires_at: Option<String>,

    // ── 旧 UserProfile 平移 ──
    pub language: String,
    pub timezone: String,
    pub city: Option<String>,
    pub country: Option<String>,
    pub occupation: Option<String>,
    pub avatar: Option<String>,
    pub builtin_avatar: Option<String>,
    pub communication_style: Option<String>,
    pub custom: HashMap<String, String>,

    // ── 生命周期 ──
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
    pub disabled_at: Option<String>,      // 软删除：保留历史 session 归属
}

pub enum Role { User, Admin }
```

**与现有 `UserProfileListFile` 的关系**——三个文件，各司其职：

| 文件 | 内容 | 加密 | 权威性 |
|---|---|---|---|
| `data_dir/accounts.json`（新增，`AccountListFile`） | **账号权威表**：`user_id` / `username` / `role` / `password_hash` / 生命周期 / 展示字段 | 明文 | **源**（multi_user 模式） |
| `data_dir/user_profiles.json`（保留，`UserProfileListFile`） | `UserProfile` 公开视图（展示字段，无 username / role / 密码） | 明文 | **派生**（Runtime `last_user_profile` 推送源） |
| `data_dir/vault/accounts/{user_id}.enc`（新增，可选） | 敏感扩展：`api_secrets` / `recovery_codes` / `encrypted_notes` | Vault master key | 扩展 |

**关键点**：`password_hash` 落在**明文**的 `accounts.json`——它是一向的 Argon2id PHC 串，本身不含明文密码，可安全落盘；放明文正是为了让登录校验在 Vault locked 时也能完成（见下）。`vault/accounts/*.enc` **不含** password_hash，只装真·机密扩展字段。`user_profiles.json` 由 `accounts.json` 派生，供 Runtime `last_user_profile` 推送（脱敏副本）。

**为什么 Argon2id 不走 Vault**：Vault 是对称加密（加密/解密需要同一把 master key），用于"短期可解密的机密"。密码哈希是**单向**的（无法反推明文），且需要在 Vault locked 状态下也能校验（典型场景：开机后用户第一次登录解锁 Vault 之前）。两者密码学属性不同，强行塞进 Vault 反而要让登录路径依赖 Vault unlock 状态（违反 §1.3 不变量 3）。

**为什么不直接用 bcrypt/scrypt**：项目 Vault 已选 Argon2id（ADR-059），保持单一 KDF 算法，减少密码学 surface。

### 决策 2：Vault 复用 — 加密"扩展敏感字段"

**布局**——账号权威表 `accounts.json`（明文）、公开视图 `user_profiles.json`（派生）、加密扩展 `vault/accounts/{user_id}.enc`（可选）：

```text
data_dir/
├── accounts.json                       # 账号权威表（明文；password_hash 为单向 PHC 串）
│   └── accounts[] = [{user_id, username, role, password_hash, display_name, ...}]
├── user_profiles.json                  # 公开视图（派生；Runtime last_user_profile 源）
│   └── users[] = [{user_id, display_name, avatar, ...}]   # 无 username / role / 密码
│
└── vault/                              # 现有 Vault 目录
    ├── salt                            # Argon2id master salt（不变）
    ├── openai.enc                      # 现有 LLM key
    └── accounts/                       # 新增子目录（可选——无扩展字段时不存在）
        ├── {user_id_1}.enc             # 加密敏感扩展字段（api_secrets / recovery_codes）
        └── ...
```

**`accounts/{user_id}.enc` 内嵌 JSON 结构**：

```json
{
  "schema_version": 1,
  "user_id": "...",
  "encrypted_notes": "...", // ChaCha20-Poly1305 加密的 note-to-self（类似 memo）
  "api_secrets": {         // 用户自己的 API key（如果有）
    "openai": "sk-...",
    "anthropic": "sk-..."
  },
  "recovery_codes": [...]  // 二次验证恢复码
}
```

**关键设计**：
- 账号创建/改密/登录**不要求** Vault unlocked——只需校验 `password_hash`。
- Vault locked 时仍可登录、可改密、可看公开 user 列表；只有"读账号加密扩展字段"才要求 unlock。
- 这恰好符合用户预期："我的密码 = 我的账号"是首要凭据，Vault 是次要保护层。

### 决策 3：HTTP 认证 — 短期 access_token + 长效 refresh_token

```text
                    ┌───────────────────────────────────────┐
                    │            Gateway                    │
                    │  POST /api/auth/login                 │
                    │   → 校验 password_hash                │
                    │   → 签发 access_token (15 min, HS256) │
                    │   → 签发 refresh_token (30 day)        │
                    │                                       │
                    │  POST /api/auth/refresh               │
                    │   → 校验 refresh_token                │
                    │   → 续签 access_token                 │
                    │                                       │
                    │  POST /api/auth/logout                │
                    │   → 撤销当前 refresh_token            │
                    │                                       │
                    │  middleware: extract AuthContext     │
                    │   → {user_id, role, as_user?}         │
                    │   → 注入到 AppState                   │
                    └───────────────────────────────────────┘
```

**Token payload（HS256 + 独立持久化签名密钥）**：

```json
// access_token
{
  "sub": "user_id_xxx",
  "role": "user" | "admin",
  "iat": 1700000000,
  "exp": 1700000900,
  "jti": "..." // 用于黑名单
}

// refresh_token
{
  "sub": "user_id_xxx",
  "token_family": "...", // 旋转检测：每次 refresh 生成新 family，旧 family 全部撤销
  "iat": ...,
  "exp": ...
}
```

**为什么 HS256 + 独立持久化签名密钥（非 Vault 派生）**：避免引入非对称密钥管理负担；签名密钥是首次启动生成的 32 字节随机 secret，持久化在 `data_dir/auth/secret`（Unix `0600`），**独立于 Vault master key**。

> **评审修订（实施期）**：原设计写的是"HMAC key 复用 Vault master key 的 SHA256 摘要"，实测发现它会让 token 签发 / 校验依赖 Vault unlocked 状态——与 §决策 1 的"登录不要求 Vault unlocked"（§1.3 不变量 3）直接冲突：Vault 一锁，所有已签发 token 无法校验、新 token 无法签发。改为独立 secret 后，Vault relock 不会踢掉在线会话，登录路径与 Vault 状态彻底解耦。签名密钥仍是 Gateway 本机机密，token 无该 secret 无法伪造，"认证锚定在 Gateway"语义不变。

**middleware 路径**：

```rust
// core/acowork-gateway/src/http/auth_middleware.rs (新文件)
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    // 1. 跳过白名单: /api/health, /api/auth/login, /api/auth/refresh
    // 2. 从 Authorization: Bearer 抽 token
    // 3. HS256 验签 + exp 检查
    // 4. payload 注入 req.extensions_mut::<AuthContext>()
    // 5. next.run(req)
}
```

**`AuthContext` 注入后下游 handler 的写法**：

```rust
pub async fn list_sessions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(params): Query<ListSessionsQuery>,
    headers: HeaderMap,
) -> Response {
    let effective_user = auth.effective_user_id(); // 普通 user = 自己; admin + as_user query = as_user
    let mut q = params;
    q.user_id = Some(effective_user);
    proxy_to_runtime_with_query(...)
}
```

### 决策 4：Session 隔离 — `SessionMeta.user_id` + Runtime `?user_id=` 过滤

**Runtime 侧改动**（最小改动）：

```rust
// core/acowork-runtime/src/conversation.rs (现有文件)
pub struct SessionMeta {
    // ... 现有字段 ...
    /// ADR-076: 创建该 session 的 user_id。None = 旧数据/系统消息（仅 admin 可见）。
    /// Immutable: 创建后不修改；admin 迁移工具可以后改，但写入路径只此一处。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}
```

```rust
// core/acowork-runtime/src/usecases/session_metadata_impl.rs (现有文件)
#[async_trait]
impl SessionMetadataService for RuntimeSessionMetadataService {
    async fn list_sessions(&self, page: u32, size: u32, user_id: Option<&str>) -> Result<...> {
        // scan_sessions_from_meta 接受可选 user_id filter
        // admin 视图 (user_id == "__admin__" sentinel) 不过滤
        ...
    }
}
```

**HTTP 路径**：

```text
GET /sessions?user_id=<uuid>&page=1&size=20
# 普通 user: user_id 强制 = self（请求里若传别的 user_id 视为 400）
# admin:     user_id 不传 = 全部; 传特定值 = 仅该 user
```

**create_session 路径**：

```rust
// core/acowork-runtime/src/agent/session/session_manager.rs
pub async fn create_session_with_id_and_conversation(
    &mut self,
    session_id: String,
    conv: Option<PathBuf>,
    committed_lines: Option<Arc<AtomicUsize>>,
    user_id: Option<String>,  // ← 新参数，从 HTTP header X-User-Id 传入
) -> Result<String> { ... }
```

HTTP 调用链：`POST /api/agents/{id}/sessions` → Gateway middleware 注入 `AuthContext` → 反代时附加 `X-User-Id` header → Runtime `create_session` 读 header 写入 meta。

**关键安全检查点**（grep 必须覆盖的清单）：
- `proxy_list_sessions`：必须把 `user_id` 注入 query
- `proxy_get_messages`：必须先 verify session.user_id == auth.effective_user_id（需要 Runtime 暴露 `GET /sessions/{sid}/owner`）
- `proxy_get_session_state` / `proxy_delete_session`：同上
- `proxy_files`：同上
- `Gateway → Runtime` 的所有 `/api/agents/{id}/sessions/*` 反代：必须经过 AuthContext 校验

**Admin `as_user` 机制**（"以 user X 视角看"，但**不是身份冒用**）：

```text
GET /api/agents/{id}/sessions?as_user=<user_id>
# admin token + as_user query → 返回该 user 的 session 列表
# 普通 user token + as_user query → 403
# 普通 user token + 不传 as_user → 仅自己的 session
```

**为什么 `as_user` 不做成"临时切换身份"**：避免 XSS / CSRF 攻击链——若前端可任意切换身份执行写操作，cookie/header 注入就能横着走。`as_user` 只用于**只读视图**（list / get_messages / get_state），写操作（POST / DELETE）永远用 token 实际身份。

### 决策 5：管理员角色 — `role = "admin"` 绕过过滤 + 特殊权限

**admin 创建流程**：
1. Gateway 首次启动时，配置文件 `gateway.toml` 含 `bootstrap_admin = { username, password }`
2. 若 `accounts.json` 为空 → 启动时强制创建该 admin 账号
3. 后续 admin 通过 admin token 创建其他 admin（需 `username` + `display_name`，密码由被创建者首次登录时设置——首次登录流程：`POST /api/auth/login?invite_token=<xxx>`）

**admin 能力清单**：
- ✅ `GET /api/users` 看到全部账号（含 `last_login_at`、`disabled_at`、但不含 `password_hash`）
- ✅ `GET /api/users/{any_id}` 看到任意账号元数据
- ✅ `GET /api/agents/{id}/sessions?as_user=<any>` 看到任意 user 的 session 列表
- ✅ `GET /api/agents/{id}/sessions/{sid}/messages?as_user=<any>` 看到任意 user 的 session 消息
- ✅ `POST /api/users/{id}/disable` 软删除账号（`disabled_at` = now）
- ✅ `POST /api/users/{id}/reset-password` 生成一次性 invite_token（24h 过期）
- ❌ 不能改他人密码（必须走 reset → 首次登录改密流程）
- ❌ 不能 `as_user` 执行写操作（POST / DELETE 仍按 token 实际身份校验）

**admin 软删除**（`disabled_at`）：保留 `user_id` 和 `session.user_id` 不变；disabled 账号的 session 仍可读（admin 视角），但 disabled 账号无法登录。注销 vs 软删除区分见决策 6。

### 决策 6：账号生命周期 — 注册 / 登录 / 改密 / 注销

**API 表**：

```text
POST   /api/auth/login              {username, password} → {access_token, refresh_token}
POST   /api/auth/refresh            {refresh_token}      → {access_token, refresh_token}
POST   /api/auth/logout             {refresh_token}      → 204
POST   /api/auth/change-password    {old_password, new_password} → 204  # 需 access_token
GET    /api/auth/me                 → UserAccount（脱敏）
POST   /api/users                   {username, display_name, password} → UserAccount
                                       # admin 创建；或开放注册模式（见下）
POST   /api/users/{id}/disable      → 204  # admin only
POST   /api/users/{id}/reset-password → {invite_token}  # admin only
POST   /api/auth/first-login        {invite_token, new_password} → {access_token, refresh_token}
DELETE /api/users/{self}            → 204  # 自己注销（软删除）
```

**注册模式开关**（gateway.toml）：

```toml
[multi_user]
registration_open = false   # 默认 false：只有 admin 可创建账号
allow_public_signup = false # 极端开放模式（仅 demo 用）
```

**注销 vs 软删除**：
- 自己 `DELETE /api/users/{self}` → `disabled_at = now`；保留所有 session、聊天记录、avatar 等（数据可被 admin 恢复）。
- admin `POST /api/users/{id}/disable` → 同上，但 admin 恢复需要二次操作。
- **不提供硬删除**：账号相关的 session / 聊天是历史数据，硬删会破坏引用完整性。

**改密强制流程**：
1. 改密：必须提供 `old_password`，新密码 Argon2id 重新哈希。
2. 改密成功后：撤销该 user 的**所有 refresh_token**（`token_family` 全杀），强制重新登录。
3. 首次登录改密：`invite_token` 单次有效（用后即焚），绑定到 `user_id`。
4. 密码策略（gateway.toml）：`min_length = 8`，`require_digit = true`，`require_mixed_case = false`。

### 决策 7：Desktop UI — 账号切换 + 侧栏 User 折叠分组

**顶栏账号菜单**（替换当前"用户偏好"入口）：

```text
[头像] 大鱼 ▾
       ├─ 切换账号...    → 弹登录 modal（清空 chatStore + 重连 MQTT）
       ├─ 修改密码      → 弹改密 modal
       ├─ 注销当前账号  → 二次确认 → DELETE /api/users/{self}
       ├─ ─────────
       ├─ 用户偏好      → 旧 UserProfile 编辑（language/timezone/avatar）
       └─ 退出登录      → 清 token + 回登录页
```

**账号切换实现**：

```ts
// apps/acowork-desktop/src/stores/authStore.ts (新)
async function switchAccount(username: string, password: string) {
  // 1. POST /api/auth/login → tokens
  // 2. localStorage["acowork.auth.tokens"] = tokens
  // 3. reset(): chatStore, agentStore, sessionStore, userProfileStore
  // 4. mqttClient.disconnect() + reconnect()（携带新 token）
  // 5. fetchAgents() / fetchUsers() / fetchSessions()
}
```

**侧栏 User 折叠分组**（[AgentList.tsx](apps/acowork-desktop/src/components/agent-list/AgentList.tsx) 同级渲染）：

```text
┌─ Agent (12) ──────────┐
│  ▶ Node A (5)         │  ← 现有 partitionAgentsByNode
│  ▶ Node B (7)         │
├─ Users (3) ───────────┤  ← 新增 partitionAccounts
│  ▶ 大鱼 (admin)       │     admin 视图下点击进入"以该 user 视角看"
│  ▶ Alice              │
│  ▶ Bob                │
└───────────────────────┘
```

**`partitionAccounts` 复用 partition 范式**（[partitionAgentsByNode.ts](apps/acowork-desktop/src/components/agent-list/partitionAgentsByNode.ts) 同结构）：

```ts
// apps/acowork-desktop/src/components/user-list/partitionAccounts.ts (新)
export function partitionAccounts(accounts: UserAccount[]): AccountGroup[] {
  // 与 partitionAgentsByNode 同形：单一折叠分组 "Users (N)"，
  // 默认折叠，点击展开；admin 视图下每行可点击触发 "以该 user 视角看 session"
}
```

**为什么 User 列表不像 Agent 那样按 Node 分组**：用户账号天然是 Gateway 维度，不存在"用户的 Node"概念——用户与 node 是正交维度（一个 admin 可以在多个 Node 上管理 agent）。强行按 Node 折叠反而制造噪音。

### 决策 8：用户-用户聊天 — Gateway 侧 conversion.json / jsonl

**存储布局**（**全部在 Gateway 机器**）：

```text
data_dir/
└── users/
    ├── {user_a_id}/
    │   ├── account.enc                    # 账号加密扩展字段（决策 2）
    │   └── chats/
    │       ├── {user_b_id}/               # min(a,b) 字典序
    │       │   ├── conversation.json      # meta（类比 SessionMeta）
    │       │   └── conversation.jsonl     # 消息流（类比 jsonl）
    │       └── {user_c_id}/
    │           └── ...
    └── {user_b_id}/
        └── chats/
            └── {user_a_id}/               # 同 min(a,b) 路径，无重复
                ├── conversation.json
                └── conversation.jsonl
```

**为什么按 `(min, max)` 字典序**：避免双向重复（A→B 和 B→A 写到同一目录），简化同步逻辑。**不**支持 group chat（超出本期范围；如需后续可扩为 `groups/{group_id}/`）。

**conversation.json schema**：

```json
{
  "schema_version": 1,
  "chat_id": "min_user_a__max_user_b",
  "participants": ["user_a_id", "user_b_id"],
  "created_at": "2026-10-15T...",
  "last_active_at": "2026-10-15T...",
  "last_message_preview": "...",
  "unread_count_a": 0,
  "unread_count_b": 3,
  "version": 42
}
```

**conversation.jsonl 一行一条**：

```json
{"ts":"2026-10-15T...","from":"user_a_id","kind":"text","body":"..."}
{"ts":"...","from":"user_b_id","kind":"image","body":"...","attachments":[{"id":"...","filename":"...","mime":"image/png","size":12345}]}
{"ts":"...","from":"user_a_id","kind":"document","body":"...","attachments":[{"id":"...","filename":"spec.pdf","mime":"application/pdf","size":67890}]}
```

**kind 集合**：`text` / `image` / `document`（**本期不支持** voice / video / reaction / edit / delete，遵循 YAGNI）。

**附件存储**：

```text
data_dir/users/{a_id}/chats/{b_id}/files/{message_id}_{filename}
```

**为什么放 Gateway 而非 Runtime**：用户聊天与 agent 无关，是 Gateway 维度的横向数据；放到 Runtime 会触发 ADR-009 §5.4 边界问题——需要为它新造 Runtime 入口，复杂且无收益。**这是 ADR-009 §5.4 的显式例外**，在本文 §5.4 显式落字。

**API**：

```text
GET    /api/users/{self}/chats                              → 聊天列表（含每个 chat 的 last_message_preview + unread_count）
GET    /api/users/{self}/chats/{other_user_id}/messages    → 该对话消息分页（offset/limit 同 ADR-050）
POST   /api/users/{self}/chats/{other_user_id}/messages    {kind, body, attachments[]} → 201 + 消息 id
POST   /api/users/{self}/chats/{other_user_id}/read        → 清空自己的 unread_count
POST   /api/users/{self}/chats/{other_user_id}/files       multipart → 上传图片/文档，返回 attachment_id
GET    /api/users/{self}/chats/{other_user_id}/files/{aid} → 下载附件
```

**Admin 权限**：
- ✅ admin 可读任意 `chats/`（应急调查需要）
- ❌ admin 不能 POST 消息冒充他人（消息 `from` 字段强制 = token.sub）
- ❌ admin 不能修改 unread_count（只能读）

### 决策 10：PM / Doc 反代身份注入 — `X-Actor` 从硬编码 `human` 改为真实 `user_id`

**现状**（单一用户假设下引入的常量，[pm_proxy.rs](core/acowork-gateway/src/http/pm_proxy.rs#L145)，[doc_proxy.rs](core/acowork-gateway/src/http/doc_proxy.rs#L145) 同构）：

| 路径 | 策略 | 注入值 |
|---|---|---|
| `/api/pm/*`、`/api/doc/*`（REST，Desktop） | 丢弃客户端自报 `X-Actor`，注入可信值 | 恒为 `"human"` |
| `/api/pm/mcp`、`/api/doc/mcp`（MCP，Agent） | 校验 `X-MCP-Actor` ∈ Gateway `installed_agents` 后透传；否则剥离（→ 匿名，仅只读工具） | agent instance_id |

**决策**："REST 面 = 人类操作面"这一安全语义**保留**，但"人类"的表示从全局单例常量升级为账号身份——反代注入值从 `"human"` 改为 `AuthContext.effective_user_id`（决策 3 的 token 身份，经 auth_middleware 注入）：

```text
POST /api/pm/projects  →  auth_middleware 解析 token → AuthContext
                       →  反代注入 X-Actor: <effective_user_id>   （不再硬编码 "human"）
```

**PM 消费端语义联动**（acowork-pm，值来自 header）：

| 消费点 | 现状（`"human"` 常量） | 多用户后（user_id） |
|---|---|---|
| `create_project.created_by` | `"human"` | 真实 user_id |
| `create_project` 自举（[tree.rs:505](core/acowork-pm/src/store/tree.rs#L505)） | `created_by != "human"` → 自动加入 members | User / Agent 创建者都自动加入 members（见决策 11） |
| `create_task` review_status（[tree.rs:762](core/acowork-pm/src/store/tree.rs#L762)） | `created_by == "human"` → NotRequired | 已登录用户创建 → NotRequired（判定按 `kind`，见决策 11） |
| `ensure_assignee_is_member`（联动指派） | `assignee ∈ ∅ ∪ members ∪ {"human"}` | `assignee ∈ ∅ ∪ members`，无 `"human"` 特例（见决策 11） |

**MCP 路径不变**：`X-MCP-Actor` 校验对象是 agent instance_id（ADR-073 身份），与用户账号正交，multi-user 不改变该校验语义。

**安全检查点**：
- `build_trusted_headers` 的 REST 分支必须从 `Extension(auth).effective_user_id` 取值，**不得**回退到常量、也不得接受客户端自报值；auth_middleware 未生效时 PM/Doc REST 反代应 401（与决策 3 的全局强制一致）。
- `X-Actor` 值域从 `"human" \| instance_id` 变为 `user_id \| instance_id`——PM 侧按值域区分，不应再出现 `"human"` 字面量；迁移期可保留兼容解析（收到 `"human"` 视为旧版 Gateway）。

### 决策 11：PM 成员模型多用户化 — 人类操作者成员化（§9 开放问题 8 决议：选 B）

**决策**：`ProjectMember` 扩展为可承载两类身份，人类操作者与 agent 成员**对称**管理，移除 `assignee = "human"` 任意人类特例。

**数据模型**（[types.rs:210](core/acowork-pm/src/types.rs#L210)）：

```rust
pub enum MemberKind { Agent, User }

pub struct ProjectMember {
    pub instance_id: String,  // 字段名兼容保留：Agent → instance_id; User → user_id
    pub kind: MemberKind,     // 新增；#[serde(default)] = Agent → 旧 project.json 零迁移
    pub added_at: DateTime<Utc>,
}
```

- **保留 `instance_id` 字段名 + 新增 `kind`**：JSON 契约不变（前端 `pm-types.ts` / `normalizeProject` 无需改字段名），`kind` 默认 `Agent` 使旧数据读出即 Agent，零迁移；语义扩展写入注释与本文档。
- **为什么显式 `kind` 而非前缀编码（`user:` / `agent:`）**：避免 user_id 与 instance_id 语义混淆；schema 迁移意图明确；与 ADR-073 三层身份 / ADR-076 user_id 维度对齐。前缀编码把类型塞进值域，破坏 UUID 可读性且无法用 serde 默认表达。

**联动指派不变式更新**：

```text
旧: task.assignee ∈ ∅ ∪ project.members ∪ {"human"}     （任意人类特例）
新: task.assignee ∈ ∅ ∪ project.members                  （人类与 agent 同构，无特例）
```

- `claim` / `submit` / `review`：actor 值域 = user_id（REST `X-Actor`）∪ instance_id（MCP `X-MCP-Actor`），必须 ∈ members——校验逻辑同一，无分支。
- `create_project` 自举（[tree.rs:505](core/acowork-pm/src/store/tree.rs#L505)）：`created_by` 无论 User 还是 Agent **都**自动加入 members。人类创建者入 members 是"人类成员化"的自洽前提（否则创建者自己无法被指派 / 认领）。
- `review_status`（[tree.rs:762](core/acowork-pm/src/store/tree.rs#L762)）：判定从 `created_by == "human"` 改为 `kind(created_by) == User` → NotRequired；Agent → Pending。

**迁移**（multi-user 上线时一次性执行）：
- 现有 `members[]` 全为 agent instance_id → `kind: Agent`（serde default 自动成立，无需数据改写）。
- `task.assignee == "human"`（旧"任意人类"指派）→ 迁移为创建者自己的 user_id；迁移期 PM 侧保留 `"human"` 兼容解析（视为旧版 Gateway，见决策 10 安全检查点）。

**与决策 10 的关系**：决策 10 解决"Gateway 注入真实身份"（`X-Actor` = user_id）；本决策解决"PM 侧消费端对称化"。配套实施、缺一不可——只做 10 不做 11，"任意人类"仍靠 `"human"` 特例兜底；只做 11 不做 10，人类成员身份无法从 header 区分。

### 决策 12：部署模式分流 — `AUTH_MODE` 由 bind 地址自动推断

**核心**：避免在单机 self-hosted 场景下强制走 multi-user 完整链路。`AUTH_MODE` 由 bind 地址自动推断（也可显式覆盖），local 模式下完整 §1-§11 决策退化为 no-op。**这不是 §5.4 的"回滚开关"——是第一类配置**。

**心智模型**（对应 [runbook §0](../runbooks/single-machine-remote-topology.md) "没有 local/remote 两套拓扑"）：架构始终一套，差异在"外部可达性" → "认证强度"。loopback-only = 物理 OS 用户管理兜底 = 信任域；LAN 暴露 = 不可信域 = 完整账号体系。

**推断规则**：

| bind 配置 | 推断 AUTH_MODE | 触发理由 |
|---|---|---|
| `127.0.0.1` / `::1`（默认） | `local` | 仅 loopback 可连 = 物理 OS 用户管理兜底 = 单机信任域 |
| `0.0.0.0` / LAN IP / 域名 | `multi_user` | 跨机/跨用户可连 = 必须完整账号体系 |
| `--auth-mode local` / `multi_user` 显式 | 覆盖推断 | 异常场景（reverse proxy 后 + 仅内网访问时强制 local；loopback 但想演示 multi_user 时强制 multi_user） |

**优先级**：CLI `--auth-mode` > TOML `[multi_user].auth_mode` > bind 自动推断 > default `local`。

**local 模式行为**（`AUTH_MODE=local`，**所有 §1-§11 决策退化为 no-op**）：
- `HttpAuth` 维持现有 bearer token（`data_dir/http_token` 文件，决策 3 的 access/refresh 不引入）
- `user_profiles.json` 维持现状（明文展示偏好），不升级为 `UserAccount` schema（user_id 字段沿用现有逻辑，不引入 password_hash / role / disabled_at）
- `SessionMeta.user_id` 字段在 write 路径**仍写入**（保持 schema 统一），但 read 路径**不过滤**——`?user_id=` query 接受但不生效
- 无 admin 角色、无 `bootstrap_admin`、无 `as_user`、`/api/auth/*` 路由不注册
- Desktop 顶栏维持现有"用户偏好"入口，不显示账号切换菜单
- 用户聊天（决策 8）不创建 `data_dir/users/` 目录；`/api/users/{self}/chats/*` 路由不注册
- PM/Doc 反代（决策 10）`build_trusted_headers` REST 分支仍注入 `X-Actor: human`（常量路径，**不**走 token 身份）
- PM 成员模型（决策 11）`assignee == "human"` 特例**保留**（与决策 11 移除特例的方向相反——但因 local 模式无 token 身份可注入，回退到原常量是唯一合理路径）

**multi_user 模式行为**（`AUTH_MODE=multi_user`）：
- 完整启用 §1-§11 所有决策
- 启动检查：`bootstrap_admin` 必须配置，**否则拒绝启动**（fail-fast；与决策 5 的"漏配告警"对冲——告警可被忽略，拒启动不可绕过）
- bind 自动调整为 `0.0.0.0`（若仍 loopback → 仅 warn，**不强制改**——便于本地演示 multi_user）

**UserAccount 在 local 模式下的存在性**（YAGNI：local 模式零改动）：
- `user_profiles.json` **维持现状**（`UserProfile` schema 不变，无 `password_hash` / `role` / `disabled_at` 字段）；凭据表 `accounts.json` **不创建**
- `AUTH_MODE=local` = 现状不变；`DISABLED_PASSWORD_HASH` sentinel **不用于 local**——它专用于 multi_user 下"admin 已创建、owner 尚未首次登录激活"的账号
- 升级 multi_user 时**一次性迁移**：`user_profiles.json` 的每个 `UserProfile` → `accounts.json` 的 `UserAccount`（`password_hash = DISABLED_PASSWORD_HASH`），owner 走 `invite_token` 首次登录设密码激活

**回滚/降级**（与 §5.4 对齐，但层级提升）：
- multi_user → local：设 `AUTH_MODE=local` 或 `--bind 127.0.0.1`；前端跳过 LoginView，token 验证回落 bearer，admin 路由不注册；账号文件保留但不可登录（`password_hash` 仍在但无 login UI 触发校验）
- local → multi_user：复用决策 6 的 `invite_token` 流程——现有 user 走"首次登录设置密码"激活；admin 创建的首 user 走同样的 invite 流程

**主流参照**：GitLab / Gitea / Jenkins / Outline / Wiki.js / Plausible 等自托管产品均按 bind/外部可访问性分流认证强度——本地模式信任物理访问，多用户模式要求完整账号体系。

**与之前决策的联动**：
- **决策 1**：`UserAccount` 字段全保留，local 模式下 `password_hash` 填 sentinel（schema 不分裂）
- **决策 5**：local 模式下 `bootstrap_admin` 不强制（首位 admin = 物理 OS 用户）；multi_user 模式下从"��配告警"升级为"漏配拒启动"
- **§5.4 回滚段**：`AUTH_MODE` 不再只是回滚段的环境变量，而是第一类配置——回滚段降级为"mode 内降级"
- **§9 开放问题 1**：local 模式下"首位 admin"概念不存在（物理 OS 用户就是 admin），multi_user 模式下保留 bootstrap_admin；问题按模式分流消解

**ponytail 标记**：bind 推断逻辑只覆盖 `127.0.0.1` vs `0.0.0.0` 二分；IPv6 link-local（`fe80::/10`）视为 multi_user（默认安全侧）。超规模场景（reverse proxy 后 + 仅内网访问）需手动 `--auth-mode local` 覆盖；bind + reverse proxy 的混合可信域判断留后续 ADR。

---

## 5. 后果

### 5.1 正面

1. **账号体系最小入侵**：复用现有 Vault master key、partition 范式、SessionMeta 持久化范式，不引入新密码学、不引入新存储层、不引入新折叠组件。
2. **session 隔离是单字段改动**：Runtime 侧只新增一个 `SessionMeta.user_id` 字段 + 一个 `?user_id=` query 参数；其它代码路径不动。
3. **admin 不破坏权限模型**：admin 是 token 内的 `role` 字段，不是身份冒用；`as_user` 严格只读，写操作按 token 真实身份校验。
4. **用户聊天是 Gateway 自有数据**：不与 Node agent 拓扑耦合；后续加 group chat / 表情反应 / 已读回执都是 schema 升级，不涉及 Runtime。
5. **token rotation**（refresh token family 旋转）：泄漏一个 refresh token 不会让攻击者永远续命——新一次 refresh 会让旧 family 全部失效。

### 5.2 负面 / 成本

1. **登录态中间件全栈改造**：所有现有 handler 必须经过 auth_middleware，handler 函数签名要加 `Extension(auth): Extension<AuthContext>`。约 30+ handler 要 touch。
2. **session 反代全链路过滤**：所有 `/api/agents/{id}/sessions/*` 反代需要把 user_id 注入 query + 二次校验 owner；漏一处 = 数据泄漏。需专门的 grep ceiling lint（见 §6）。
3. **Desktop 双 store 并存过渡期**：`userProfileStore`（旧） + `authStore`（新）共存一段时间，迁移期两者数据可能不一致；需要清晰 deprecation 路径。
4. **token 撤销的存储成本**：refresh token family 需要持久化以支持"该 family 全部撤销"语义，单独文件或 Redis；本期选文件（`data_dir/auth/revoked_families.txt`），量小可接受。
5. **首次启动门槛（multi_user 模式）**：必须通过 `bootstrap_admin` 配置创建首位 admin；漏配导致系统空跑无人能登录——**fail-fast 拒启动**（决策 12 升级），而非仅告警。local 模式无此门槛（首位 user 由 `bootstrap/orchestrator.rs` 自动创建，见决策 12 "UserAccount 在 local 模式下的存在性"）。

### 5.3 边界 / 例外

**ADR-009 §5.4 的扩展条款**（在 ADR-009 v3 修订时追加）：

> **例外 — 用户账号与聊天数据**（ADR-076 §决策 8）：账号凭据（`data_dir/vault/accounts/`）、账号元数据（`data_dir/user_profiles.json`）、用户-用户聊天记录（`data_dir/users/*/chats/`）归属 Gateway 进程，由 Gateway 直接读写 fs。这些数据与任何 agent instance 无对应，不属于"Runtime 私有数据"。运行时 session 数据仍只通过 Runtime HTTP 反代访问，本例外仅限上述三类数据。

### 5.4 回滚

**模式级回滚**（第一类，由 §决策 12 的 `AUTH_MODE` 控制）：
- multi_user → local：设 `AUTH_MODE=local` 或 `--bind 127.0.0.1`；前端跳过 LoginView，token 验证回落 bearer，admin 路由不注册；账号文件保留但不可登录（密码哈希仍在但无 login UI 触发校验）；`SessionMeta.user_id` 写入但不过滤；用户聊天路由不响应；PM/Doc REST 反代回退到 `X-Actor: human` 常量注入
- local → multi_user：复用决策 6 的 `invite_token` 流程——现有 user 走"首次登录设置密码"激活；admin 创建的首 user 走同样的 invite 流程

**字段级回滚**（第二类，mode 内的细粒度退化）：
- `UserProfile` → `UserAccount` 是 in-place 升级（迁移脚本同表字段），回滚 = 删 `user_id` / `password_hash` 字段
- `session.user_id` 字段可选，删除后过滤失效（回到所有人共享）——**仅 multi_user 模式相关**，local 模式本就不过滤
- auth middleware 可选：保留 `HttpAuth` bearer token 兜底路径
- 聊天数据独立目录，删除 `data_dir/users/*/chats/` 即视为"未启用用户聊天"
- PM/Doc 反代身份（决策 10）：multi_user 模式下 `build_trusted_headers` 注入 `auth.effective_user_id`；local 模式注入常量 `human`（决策 12 联动）
- PM 成员模型（决策 11）：`ProjectMember.kind` 带 `#[serde(default)]`，回滚仅丢弃 kind 字段、数据文件 schema 兼容；`"human"` 兼容解析保留到迁移完成后再移除（multi_user 模式路径，local 模式保留 `"human"` 常量路径）

### 5.5 已知技术债

- **ponytail: HS256 token 校验是 CPU 同步操作**，无状态撤销，黑名单靠家族检测。下一步：迁移到 RS256 + Redis 黑名单（用户量 > 100 时）。
- **ponytail: 用户聊天列表是 fs 扫描**，O(n) on `data_dir/users/`。n < 1000 时可接受；超过需要 SQLite 索引。
- **ponytail: `as_user` query 在反代链路上是字符串透传**，未来如果引入 proto 升级需要结构化字段。
- **未实现**：多设备登录并发 session 限制、密码过期强制改密（本期仅记录 `password_expires_at` 不强制）、账号 lockout（5 次失败 → 15 分钟锁定）——这些放后续 ADR。

---

## 6. 改动清单（按 crate / 文件）

### 6.1 core/acowork-core

- 新增 `src/account.rs`（**已实现**）：`UserAccount`、`Role`、`AccountListFile`、`DISABLED_PASSWORD_HASH`；`UserAccount::to_public_profile()` 产出 `UserProfile` 公开视图
- 修改 `src/protocol.rs`：保留 `UserProfile`（展示用），新增 `AccountPublicView`（脱敏后的 API 返回类型）
- 修改 `Cargo.toml` 依赖（如需 jsonwebtoken crate）

### 6.2 core/acowork-vault

- **零改动**：复用 `Vault::store` / `Vault::retrieve` 接口，仅新增调用方（account 模块）

### 6.3 core/acowork-runtime

- 修改 `src/conversation.rs`：`SessionMeta` 加 `user_id: Option<String>` 字段
- 修改 `src/usecases/session_metadata_impl.rs`：`list_sessions` 接受 `user_id: Option<&str>` filter；admin 时（sentinel `"__admin__"`）不过滤
- 修改 `src/http/server.rs`：`/sessions` 接受 `?user_id=` query
- 修改 `src/agent/session/session_manager.rs`：`create_session_with_id_and_conversation` 接受 `user_id` 参数，从 HTTP header `X-User-Id` 读
- 修改 `src/agent/session/restorer.rs`：恢复历史 session 时保留 `user_id` 字段

### 6.4 core/acowork-gateway

**新增**：
- `src/http/auth_middleware.rs`：token 校验 + `AuthContext` 注入
- `src/http/auth_api.rs`：`/api/auth/login` `/refresh` `/logout` `/change-password` `/first-login` `/me`
- `src/http/account_api.rs`：账号 CRUD（替换并扩展现有 `users_api.rs`）
- `src/http/chat_api.rs`：用户-用户聊天 API
- `src/account/store.rs`（已实现）：账号权威表 `accounts.json` 读写（原子写：temp + rename）；`src/account/password.rs`（已实现）：Argon2id PHC 哈希 / 校验
- `src/auth/token.rs`（已实现）：HS256 token 签发 / 校验 / refresh family 管理；签名密钥 `data_dir/auth/secret`（首次启动生成，`0600`）
- `src/auth/revoked.rs`（已实现）：`revoked_families.txt` 文件管理（精确 family + `{user_id}.*` 通配，后者用于改密全杀）
- `src/chat/persistence.rs`：`conversation.json` + `conversation.jsonl` 读写
- `src/chat/attachments.rs`：图片 / 文档附件落盘（`data_dir/users/.../files/`）
- `src/auth/mode.rs`：`AUTH_MODE` 推断 + bind 地址解析 + CLI flag 解析（决策 12）；`pub enum AuthMode { Local, MultiUser }`；`pub fn resolve_auth_mode(cli: &CliArgs, toml: &TomlConfig) -> AuthMode`；pub `is_loopback_bind(addr: &SocketAddr) -> bool` helper

**修改**：
- `src/http/routes.rs`：`AppState` 加 `auth_middleware`（**仅 multi_user 模式生效**，local 模式注册 legacy `HttpAuth` bearer middleware）；`AppState` 加 `auth_state: Arc<AuthState>` + `auth_mode: AuthMode`；router 注册时分流——multi_user 模式加载 `/api/auth/*` `/api/users/*/chats/*` admin 路由，local 模式全部不注册
- `src/http/proxy.rs`：所有 `proxy_*_sessions*` 函数加 `Extension(auth)`，把 `effective_user_id` 注入 query / header；新增 `?as_user=` 处理（仅 admin + 仅 GET）；**local 模式跳过 user_id 过滤**（query 接受但不修改 Runtime 行为）
- `src/http/pm_proxy.rs` / `src/http/doc_proxy.rs`（决策 10）：`build_trusted_headers` 签名加 `auth: &AuthContext` 参数；multi_user 模式 REST 分支从注入常量 `"human"` 改为 `auth.effective_user_id`；**local 模式保留 `X-Actor: human` 常量注入**（决策 12 联动）；MCP 分支 `X-MCP-Actor` 校验逻辑不变（两模式一致）
- `src/http/users_api.rs`：**废弃**，合并到 `account_api.rs`（保留兼容路径 → `account_api` 的 alias）；**local 模式下路由 alias 也不注册**
- `src/resource_cache.rs`：`UserProfileListFile` 改名或保留——保留 `user_profiles.json` 作为公开视图；local 模式下维持现有 `UserProfile` 写入路径（不引入 `password_hash` sentinel）
- `src/account/store.rs`（决策 2 配套）：**local 模式不创建 `accounts.json`**，不碰 `user_profiles.json`（零改动）
- `src/bootstrap/orchestrator.rs`：**multi_user 模式**：检查 `bootstrap_admin` 配置，若 accounts 为空则强制创建首位 admin（**缺配则拒启动**——fail-fast，决策 12 升级原决策 5 的"告警"为"拒启动"）；**local 模式**：**零改动**（不碰 `accounts.json`，不创建任何 `UserAccount`）
- `src/config.rs`：新增 `[multi_user]` 段：`registration_open`、`password_policy`、`bootstrap_admin`；新增 `auth_mode: Option<AuthMode>` 字段（`None` = bind 自动推断；`Some(Local)` / `Some(MultiUser)` = 显式覆盖）
- `src/cli.rs`（CliArgs）：新增 `--auth-mode <local|multi_user>` flag；与现有 `--addr` 联动——若用户显式传 `--auth-mode` 与 `--bind` 冲突 → warn 但尊重 `--auth-mode`

### 6.5 apps/acowork-desktop

**新增**：
- `src/stores/authStore.ts`：token 管理 + 账号切换 reset 流程
- `src/components/account-switcher/`：顶栏账号菜单 + 登录 modal + 改密 modal + 注册 modal（admin 可见）
- `src/components/user-list/UserList.tsx` + `partitionAccounts.ts`：侧栏 User 折叠分组
- `src/components/chat/`：用户-用户聊天 UI（列表 / 会话 / 附件上传）
- `src/lib/api/auth.ts`：HTTP client 注入 Authorization header

**修改**：
- `src/components/agent-list/AgentList.tsx`：在 agent 分组下方插入 `<UserList />`
- `src/stores/userProfileStore.ts`：**保留**（展示用），新增 `authStore` 作为更高层（auth state 决定当前 userProfile；userProfile 是脱敏副本）
- `src/App.tsx`：登录态判断；未登录 → 渲染 LoginView；登录后 → 现有主界面
- `src/lib/types.ts`：新增 `UserAccount`、`AuthState`、`Role` 类型
- `src/i18n/`：新增 `account.*` / `userList.*` / `chat.*` 文案键

### 6.6 dev/ci.sh 新增 ceiling lint

```bash
# 防止遗漏 auth_middleware 的 handler
grep -rn "Extension(auth)" core/acowork-gateway/src/http/ | wc -l
# 防止 proxy 反代遗漏 user_id 注入
grep -rn "proxy_list_sessions\|proxy_get_messages\|proxy_get_session_state" core/acowork-gateway/src/http/proxy.rs
```

### 6.7 core/acowork-pm（决策 10 + 11 联动）

- `src/types.rs`：`ProjectMember` 新增 `kind: MemberKind`（`Agent`/`User`，`#[serde(default)] = Agent`）
- `src/store/tree.rs`：`ensure_assignee_is_member` 移除 `"human"` 特例；`create_project` 自举改为 User / Agent 创建者都自动加入 members；`create_task` 的 `review_status` 判定从 `== "human"` 改为按 `kind`
- `src/api/tasks.rs` / `src/api/projects.rs`：actor 解析统一支持 user_id（REST `X-Actor`）与 instance_id（MCP `X-MCP-Actor`）双值域
- `src/mcp/agent_dir.rs`：成员校验沿用 `agent_exists` 兜底；User 成员不做 MCP 校验（agent 工具面不面向人类成员）
- 测试：`tests/handlers_e2e.rs` 的 "human 建项目 members 为空" 断言改为 "人类创建者入 members"（行为契约随决策 11 变更）

---

## 7. 测试策略

### 7.1 单元测试

- `core/acowork-gateway/src/account/`: 账号创建 / 改密 / 注销 round-trip；Vault locked 状态下元数据可见
- `core/acowork-gateway/src/auth/token.rs`: HS256 签发 / 校验；refresh family rotation；篡改 token 拒绝
- `core/acowork-gateway/src/auth/revoked.rs`: revoke family 后旧 refresh_token 失效
- `core/acowork-runtime/src/conversation.rs`: `SessionMeta` 序列化含/不含 `user_id` 兼容（无字段的旧 jsonl 仍可加载）
- `core/acowork-runtime/src/usecases/session_metadata_impl.rs`: `list_sessions(user_id=Some(x))` 只返该 user 的 session；admin 时返全部

### 7.2 集成测试（e2e）

- 完整流程：admin 创建 → alice 创建 → alice 创建 session → bob 看不到 alice 的 session → admin 用 `?as_user=alice` 看到
- 改密流程：alice 改密 → 旧 refresh_token 失效 → 必须重新 login
- 注销流程：alice DELETE self → alice 无法再 login → 历史 session 仍可被 admin 读
- 用户聊天：A → B 发文字 + 图片 + 文档 → B 收到 → unread_count 增加 → B read 后清零
- Vault locked 流程：Vault 锁上后 alice 仍可登录（`password_hash` 在明文的 `accounts.json`），但 admin 想读 `vault/accounts/*.enc` 时报错

### 7.3 协议兼容性

- `SessionMeta.user_id` 字段为 `Option<String>` + `skip_serializing_if`：旧 meta.json 无此字段 → 加载为 `None`（行为 = 旧版）
- `user_profiles.json` 保留兼容：现有 `UserProfile` 字段不动，新增字段（如 `username` / `role`）作为可选

### 7.4 安全测试（手动 checklist）

- [ ] 普通 user 用 `?as_user=<admin>` 访问 → 403
- [ ] 普通 user 改 `X-User-Id` header → Runtime 侧与 token 不一致 → 拒绝
- [ ] 注销账号后旧 refresh_token → revoked_families 命中 → 拒绝
- [ ] admin 用 `as_user` 调 POST（写操作）→ 403
- [ ] 跨用户聊天路径：`POST /api/users/{A}/chats/{B}/messages` 以 A 身份发送，从 token 拿 from 字段 → from=B 拒绝

### 7.5 部署模式测试（决策 12）

**单元测试**（`core/acowork-gateway/src/auth/mode.rs`）：

- `resolve_auth_mode` 真值表：
  - `bind = 127.0.0.1:19876`，无显式 flag → `Local`
  - `bind = 0.0.0.0:19876`，无显式 flag → `MultiUser`
  - `bind = 192.168.1.20:19876` → `MultiUser`
  - `bind = [::1]:19876` → `Local`
  - `bind = [fe80::1]:19876` → `MultiUser`（link-local 默认安全侧）
  - 显式 `--auth-mode local` + `bind = 0.0.0.0` → `Local`（显式覆盖 bind）
  - 显式 `--auth-mode multi_user` + `bind = 127.0.0.1` → `MultiUser`（显式覆盖 bind，仅 warn）
- 优先级链：CLI > TOML > bind 推断 > default

**集成测试**（`core/acowork-gateway/tests/auth_mode_e2e.rs`，新增）：

- **local 模式启动**：bind `127.0.0.1` + 无 `bootstrap_admin` → 正常启动；`user_profiles.json` 含 1 条 `role = User` / `password_hash = "$disabled$"` 的记录；`GET /api/auth/login` → 404（路由未注册）；`GET /api/users/{self}/chats` → 404
- **multi_user 模式启动**：bind `0.0.0.0` + 无 `bootstrap_admin` → **拒启动**（exit code != 0，stderr 含 `bootstrap_admin` 提示）
- **multi_user 模式启动**：bind `0.0.0.0` + 配 `bootstrap_admin` → 正常启动；`GET /api/auth/login` 可达
- **local 模式 session 不过滤**：创建 2 个 session 绑定不同 `user_id` → `GET /sessions` 返回全部 2 条（不过滤）
- **multi_user 模式 session 过滤**：同上场景 → 普通 user token 只返回自己的 1 条；admin token 返回 2 条

**回归防护**（`dev/ci.sh` 新增 ceiling lint）：

```bash
# 决策 12：local 模式下不得注册 auth/admin 路由——防止后续 PR 误把 multi_user 路由变成无条件注册
grep -rn "auth_middleware\|account_api::router" core/acowork-gateway/src/http/routes.rs \
  | grep -v "auth_mode\|AuthMode" && echo "FAIL: 路由注册未按 auth_mode 分流" && exit 1

# 决策 12：local 模式下 `accounts.json` 不得创建
grep -rn "save_accounts\|account_list_path" core/acowork-gateway/src/ | grep -v "auth_mode\|auth::mode\|AuthMode" \
  && echo "FAIL: accounts.json 写入未按 auth_mode 分流" && exit 1
```

---

## 8. 实施里程碑（建议）

| Phase | 内容 | 估时 |
|---|---|---|
| 1 | `UserAccount` 数据模型 + Argon2id password_hash + Vault 加密扩展字段 | 1 周 |
| 2 | `/api/auth/*` + token middleware + `authStore` + 顶栏账号菜单（登录 / 改密 / 注销） | 1 周 |
| 3 | `SessionMeta.user_id` + Runtime `?user_id=` 过滤 + 反代全链路注入 | 1.5 周（含 grep ceiling lint） |
| 4 | admin 角色 + `as_user` + bootstrap_admin + 首位 admin 创建 | 0.5 周 |
| 5 | Sidebar User 折叠分组（`partitionAccounts` + UserList.tsx） | 0.5 周 |
| 6 | 用户-用户聊天（persistence + API + Desktop UI + 附件上传） | 2 周 |
| 7 | ceiling lint + 集成测试 + 文档（README / 用户手册） | 1 周 |

总计 ~7.5 周。建议分两个 PR 合并：**PR1 = Phase 1-4**（账号 + 隔离 + admin），**PR2 = Phase 5-6**（UI + 聊天），**PR3 = Phase 7**（lint + 文档）。

---

## 9. 开放问题（评审请重点看）

> **状态更新**：1-7 已按主流技术路线决议（行业标准 + "做就做最好的"原则），作为设计输入纳入决策；问题 8 已在 §决策 11 决议。
>
> **模式分流补充（§决策 12）**：本地（bind `127.0.0.1`）部署下，**§1-§11 整套决策退化为 no-op**——问题 1（首位 admin = 物理 OS 用户）、问题 2（注销策略 = OS 账户注销）、问题 3-7（隔离/聊天/原子性/并发 = 单用户场景下不触发）按 `AUTH_MODE=local` 分流消解，不需单独决议。multi_user 模式（bind `0.0.0.0`）下问题 1-7 才进入实施路径。

1. **✅ 已决议 — 首位 admin 创建流程**：选 **`bootstrap_admin` 配置驱动**（无人值守优先）。理由：Kubernetes / Consul / etcd / Docker 等集群系统均采配置文件 / 环境变量方式；"Gateway 是 keep-alive 进程不应有 stdin" 是既有架构原则（`AGENTS.md` 已明确）。交互式场景由**独立 CLI 子命令** `acowork-gateway admin create` 提供（独立进程，不破坏 keep-alive 边界），对应 [apps/cli/](apps/cli/) 增量。配置缺失时启动检查 + 日志告警（决策 5 已有）。
2. **✅ 已决议 — 注销策略**：本期**只软删除**（`disabled_at`，与决策 6 一致），不提供硬删除。理由：Slack / Discord / Teams 均默认软删除保留历史；硬删除仅在 GDPR 等法律强制场景需要（会破坏 session / 聊天的引用完整性），本期 YAGNI；后续若需硬删除再立独立 ADR（带宽限期 + 异步清理任务 + 引用重映射策略）。
3. **✅ 已决议 — 群聊（group chat）**：`conversation.json` 的 `participants` 字段类型保留 `Vec<String>`，本期运行时断言 `len() == 2`；未来 group chat 通过 "len() > 2 + group metadata（name / avatar / owner）" 扩展，**零迁移**（schema 已兼容）。理由：Slack / Discord / 微信 / Telegram 均采用 DM / Group 统一 schema；本期前端仅暴露 2 人路径，schema 留口。
4. **✅ 已决议 — 聊天附件大小限制**：**图片 25 MB / 文档 100 MB / 不引入 virus scan**。ponytail 标记：个人 / 小团队场景下 trade-off 已知；超过此规模需引入 ClamAV（独立进程）+ 对象存储分拆（独立 ADR）。理由：Discord 25 MB（图片 / 视频）、Telegram 100 MB（任意文件）是公认的 sweet spot；virus scan 在用户量 < 100 时 ROI 为负，是 over-engineering。
5. **✅ 已决议 — `as_user` 是否需要写操作**：本期**只读视图**（与决策 4 一致），写操作保持 token 实际身份。理由：避免 XSS / CSRF 攻击链（决策 4 已分析：身份冒用 = 横向越权入口）。若运营场景需要"admin 代用户发消息给 agent"，按 **OAuth 2.0 Token Exchange (RFC 8693)** 模式扩展——新增 `X-On-Behalf-Of` header + `act` claim + 单独 ACL 策略，**独立 ADR** 设计，不污染本期 schema。
6. **✅ 已决议 — Desktop 账号切换的原子性**：保留 ADR 原建议——**保留失败时的"已登出但未登入"中间态**，UI 引导用户重新登录（LoginView 直接渲染）。理由：VS Code / Google 账号切换均采用此模式；事务性账号切换是过度设计，失败回滚反而引入新的不一致风险（旧 token 已撤销 / 新 token 没拿到 / MQTT 状态半连接——三者纠缠）。
7. **✅ 已决议 — 多设备并发**：本期**允许多设备并发**（每设备独立 `token_family`），不引入 `device_id` 与并发上限。理由：Slack / Discord / Google / Microsoft 均默认支持多设备并发；OAuth 2.0 RFC 6749 本身不限制。后续若需要按 `device_id` + `max_sessions_per_user` + "新登录踢旧登录"策略扩展，立独立 ADR。
8. **~~PM `"human"` 特例的演化~~ ✅ 已决议（选 B）**：人类操作者成员化——`ProjectMember` 新增 `kind`（`Agent`/`User`），人类与 agent 成员对称，`assignee ∈ ∅ ∪ members` 无特例。完整设计见 **§决策 11**。~~选项 A（放宽为"任意已登录用户可被指派"）~~ 已拒绝：语义模糊（A 指派的活 B 可认领），且与 ADR-073 三层身份范式不对齐。

---

