# ADR-090: MCP/LSP Server 安装归属下沉到 Node——preset 只读、runtime 文件为唯一真相

**状态**：提议（待架构评审）
**日期**：2026-10-09
**决策者**：待评审

**关联**：
- [ADR-055](./ADR-055-remote-runtime-node-topology.md)（§6.1 职责表 ③、§3 L2 数据布局——本 ADR **修正**其「MCP 属 Gateway 全局资源」的定性）
- [ADR-072](./ADR-072-mcp-install-package-spec.md)（声明式 `McpPackageSpec`——本 ADR **继承**其决策 1/2/4/5/6/7，**取代**其决策 3）
- [ADR-069](./ADR-069-mcp-tool-level-optin.md)（MCP 工具级 opt-in——`active_server_names` 语义不变）
- [ADR-089](./ADR-089-lsp-relay-reachability-vs-exposure.md)（D2 反代路由模式——本 ADR **复用**，不重复设计）
- [ADR-019](./ADR-019-lsp-relay-standalone-process.md) / [ADR-030](./ADR-030-sidecar-endpoint-dynamic-push.md)（LSP relay 进程与端点推送）

---

## 1. 决策摘要

### 1.1 一句话

MCP 与 LSP server 的**安装是 Node 本地事件**（stdio server 由 Runtime `fork/exec`，二进制必须同机），因此 **server 列表的权威源在 Node，不在 Gateway**。
Gateway 退化为**纯反代 + 调度**：不再持有任何 MCP/LSP server list，UI 一律先选 Node、再按 Node 读写。
安装目录的 preset 文件**只读**，仅作初始化数据来源；`{node_data_dir}/mcp_servers.json` 与 `{node_data_dir}/lsp_servers.json` 是**唯一运行时真相**（preset ∪ custom ∪ install state）。

### 1.2 关键决策表

| # | 决策 | 内容 |
|---|------|------|
| D1 | preset 只读、随套件安装 | `assets/mcp_presets.json` + `assets/mcp_install/`，与 `assets/lsp_servers.json` + `assets/lsp_install/` 同构。运行时 MUST NOT 写这两个文件；**不提供**用户级 preset override 文件（原开放问题 1 定为不允许） |
| D2 | runtime 文件为唯一真相 | `{node_data_dir}/mcp_servers.json`、`{node_data_dir}/lsp_servers.json`——preset ∪ custom ∪ install state 三合一，单文件、`IndexMap` 保序 |
| D3 | 同名不冲突 | runtime 文件与 preset 文件**同名**（`lsp_servers.json`），靠路径域区分（安装目录 vs `{node_data_dir}`），不引入 `_state` 后缀 |
| D4 | Node 平铺，不开 `data/` 子目录 | 遵循 `{node_data_dir}` 现有约定（`identity.json`、`state.json`、`packages/`、`logs/`、`runtime-logs/` 全平铺），新增两个 JSON 同层放置 |
| D5 | Gateway 无 server list | 删除 `core/acowork-gateway/src/http/mcp_catalog_api.rs`，`/api/mcp-catalog*` 全部路由移除。UI 读取走 `/api/nodes/{node_id}/mcp/servers`（复用 ADR-089 D2 反代） |
| D6 | 移除 agent 级安装能力 | 删除 Runtime 内置 `mcp_install` / `mcp_uninstall` tools。Agent **只使用** MCP，不安装 |
| D7 | Runtime 直读本机文件 | Runtime 启动读 `{node_data_dir}/mcp_servers.json`（新增 `--node-data-dir`），不再依赖 Gateway `RuntimeConfigUpdate` 推送 catalog |
| D8 | `source` 字段持久化 | 每条目携带 `source: "preset" \| "custom"`，用于 UI 分组与 preset 升级判定 |
| D9 | 合并规则：元数据刷新、实例与状态保留 | preset 升级刷新 `kind/spec/description/install_hint/category`；保留用户 `env/spawn_args/exec_override` 与 `install_state` |
| D10 | 不做 reset-to-preset | 不提供「恢复 preset 默认值」入口；用户改错自行改回 |
| D11 | 文件权限 0600 + 原子写 | runtime 文件含 API key 明文，MUST `mode 0600`；写入 MUST `tmp + fsync + rename` |
| D12 | LSP 同构改造 | LSP 补齐 custom server 与 install state 持久化（现状二者皆缺），与 MCP 共用同一套加载/合并/写入约定 |
| D13 | custom server 的 install 脚本 = 设置界面粘贴 | 实例段新增 `install_script`（内联脚本文本，非文件名）。用户在 MCP/LSP 设置界面粘贴脚本内容，Node 侧用 `run_command_with_idle_timeout` 执行（沿用 LSP 的 idle-timeout 机制）。preset 的 `install_script` 仍是 `assets/mcp_install/` 下的**文件名**；两者按 `source` 区分解析 |
| D14 | preset 与 LSP/MCP 文件不合并 | `assets/mcp_presets.json` 与 `assets/lsp_servers.json` 保持两个文件（原开放问题 2 定为不需要合并）——schema 差异大（MCP 有 `env`/`required_env`，LSP 有 `candidates`/`root_markers`），合并只会产生大量 `Option` 字段 |

---

## 2. 背景与现状事实

### 2.1 MCP 安装的真实语义（非「纯注册」）

`core/acowork-mcp/src/install.rs` 的 install 做三件事：`probe_runtime()`（查 PATH）→ `run_install()`（真跑 `pipx/cargo/go install`、`docker`、`assets/mcp_install/*.sh`）→ `health_check()`（spawn 派生命令，initialize → list_tools）。

按 `PackageKind` 分两类，**决定远程 Node 的处理方式**：

| kind | `derive_install_command()` | 副作用位置 | 远程可行性 |
|------|---------------------------|-----------|-----------|
| `npm` / `pypi`+`uvx` | `None`（`npx -y` / `uvx` 每次 fresh resolve） | 无（或临时缓存） | 仅需推 spawn config |
| `pypi`+`pipx`/`pip`、`cargo`、`go`、`docker`、`script` | 有 | `~/.local/pipx/venvs/`、`~/.cargo/bin/`、`~/go/bin/`、镜像本地缓存 | **必须在 spawn 发生的那台机器执行** |

这是 D5/D7 的根因：安装产物是文件系统事实，只有 Node 知道。

### 2.2 preset 目前只存在于前端

8 个内置 preset 硬编码在 `apps/acowork-desktop/src/lib/mcp-presets.ts`（`playwright`、`computer-use`、`brave-search`、`exa-search`、`context7`、`docling`、`notion`、`figma`；category：browser / search / document / knowledge / design）。

后果：

1. Rust 侧零内置——Gateway 收到 `POST /api/mcp-catalog/install` 时才第一次看到 spec 全量，无法预先 probe / 校验。
2. 非 Desktop 客户端（CLI、第三方）无 preset 可用。
3. `assets/` 目录**无任何 mcp 相关文件**（现状仅 `lsp_servers.json`、`lsp_install/`、`offline_providers.json`、`offline_embedding_providers.json`）。

### 2.3 LSP 的物理链路（本 ADR 的对照物）

| 阶段 | 位置 |
|------|------|
| preset 元数据 | `assets/lsp_servers.json`（151 行，`version: 1`，`IndexMap<String, LspServerEntry>`） |
| install 脚本 | `assets/lsp_install/{rust,python,typescript,go,clangd,json,yaml,markdown,kotlin,java,css,html}.{sh,ps1}` |
| dev 拷贝 | `apps/acowork-desktop/src-tauri/build.rs:142-156` → `target/{profile}/bin/` |
| release 打包 | `apps/acowork-desktop/src-tauri/tauri.conf.json:58` `bundle.resources` 映射 |
| 定位注入 | `apps/acowork-desktop/src-tauri/src/commands/gateway.rs:545` `env("ACOWORK_LSP_CONFIG_DIR", resource_dir)` |
| 加载候选 | `core/acowork-lsp-relay/src/config.rs::build_config_candidates()`：`$ACOWORK_LSP_CONFIG_DIR` → `$CARGO_MANIFEST_DIR/../../assets` → `exe_dir` → `cwd` |
| fallback | `builtin_lsp_defaults()`（`config.rs:239`，**非** `#[cfg(test)]`，是 JSON 缺失时的兜底） |

ADR-072 已把该机制声明为「对照物」，但只移植了**命令推导矩阵**，未移植**资产布局**。

### 2.4 LSP 现状缺的两项能力

1. **不能新增 server**——只能 override `lsp_servers.json` 已有 key，故「preset ∪ custom 合并」问题从未暴露。
2. **install state 不持久化**——状态存内存 `RwLock<LspServersWithStatus>`，每次重启重新探测。MCP 不能照此办理：`pipx install` 耗时分钟级，重启即重跑不可接受。

### 2.5 `{node_data_dir}` 现状

`core/acowork-node/src/cli.rs:60-62`：`--data-dir`（alias `--work-dir`，env `ACOWORK_NODE_HOME`），默认 `$HOME/.acowork/acowork-node`。实测内容：

```
identity.json  state.json  packages/  logs/  runtime-logs/
```

无 `data/` 子目录。D4 据此选择平铺。

---

## 3. 备选方案

| 方案 | 描述 | 否决理由 |
|------|------|---------|
| A. Gateway 持 registry + MQTT 派发 install | Gateway 保留 `mcp_catalog.json`，install 经 MQTT 派发到 Node 执行 | 双源（Gateway registry + Node 实态）必然漂移；Node 掉线时 Gateway 状态不可信；Desktop 单机模式下二者同机，白增一层 |
| B. MCP-as-a-Service（Node 常驻 MCP 网关进程） | 仿 lsp-relay，MCP 走 RPC | MCP 是双向长连接 JSON-RPC（tool call / list_resources / subscribe），非 LSP 式 initialize 一次性握手；拆 service 需重写 transport 层 |
| C. 仅 `npm`/`uvx` 支持远程，其余锁 Gateway 同机 | 按 kind 分能力 | 与目标（Runtime 漂到远程 Node 仍可用 MCP）直接冲突 |
| D. 两卡片 UI（registry 卡 + per-node 已装卡） | 元数据与状态分屏 | server 列表本就在 Node 上，只有「Node 视图」一种形态；分屏制造「全局 registry」错觉。LSP 单表 + status 列已验证足够 |
| **E. 本 ADR** | preset 只读 + Node runtime 文件唯一真相 + Gateway 纯反代 | — |

---

## 4. 决策理由

1. **安装与 spawn 同机是硬约束**。stdio MCP server 由 Runtime `fork/exec`，二进制必须在同一文件系统。故「装了没装」是 Node 的属性，不是 Agent 的、更不是 Gateway 的。
2. **单源优于双源**。方案 A 需持续对账；本模型下 Gateway 无状态，读即转发 Node，天然一致。
3. **UI 的 Node 过滤是既成事实**。MCP/LSP 设置界面已按 Node 过滤（与 Node 一对一），「Gateway 有全局 list」在 UI 上没有消费者。
4. **LSP 链路已被验证**。资产布局、打包、候选路径、fallback 全有现成实现，D1/D3 是照抄而非发明。

---

## 5. 详细设计

### 5.1 文件布局

```
安装目录（只读，随套件分发）
  assets/mcp_presets.json          # 新增，8 个内置 preset
  assets/mcp_install/*.sh|*.ps1    # 新增，script kind 兜底
  assets/lsp_servers.json          # 现状不变
  assets/lsp_install/*.sh|*.ps1    # 现状不变

{node_data_dir}/                   # $HOME/.acowork/acowork-node（D4 平铺）
  identity.json                    # 现状
  state.json                       # 现状
  mcp_servers.json                 # 新增，唯一运行时真相（D2）
  lsp_servers.json                 # 新增，与 preset 同名（D3）
  packages/  logs/  runtime-logs/  # 现状
```

preset 定位沿用 LSP 的候选顺序：

```
1. $ACOWORK_MCP_CONFIG_DIR/mcp_presets.json
2. $CARGO_MANIFEST_DIR/../../assets/mcp_presets.json   (dev/test)
3. {exe_dir}/mcp_presets.json                          (release)
4. {cwd}/mcp_presets.json                              (dev convenience)
5. builtin_mcp_defaults()                              (兜底，仅 1-2 条核心 preset)
```

Desktop 已把 `resource_dir` 经 `ACOWORK_LSP_CONFIG_DIR` 传给 Gateway；`mcp_presets.json` 落在同一目录，P7 打包时复用该路径，MUST NOT 新增第二个 env var（除非需要分离，届时另案）。

### 5.2 schema

**preset（只读，`assets/mcp_presets.json`）**——字段即 ADR-072 `McpPackageSpec` + 展示字段：

```json
{
  "version": 1,
  "presets": {
    "brave-search": {
      "kind": "npm",
      "spec": "@modelcontextprotocol/server-brave-search",
      "runner": null,
      "entry_point": null,
      "spawn_args": [],
      "exec_override": null,
      "install_script": null,
      "http_probe_ports": [],
      "transport": "stdio",
      "description": "Privacy-first web & local search via Brave Search API.",
      "install_hint": "Get free API key at https://brave.com/search/api/",
      "required_env": ["BRAVE_API_KEY"],
      "optional_env": {},
      "category": "search"
    }
  }
}
```

**runtime（读写，`{node_data_dir}/mcp_servers.json`）**——每条目三段：

```json
{
  "version": 1,
  "servers": {
    "brave-search": {
      "source": "preset",

      "kind": "npm",
      "spec": "@modelcontextprotocol/server-brave-search",
      "runner": null,
      "entry_point": null,
      "transport": "stdio",
      "url": null,
      "description": "...",
      "install_hint": "...",
      "category": "search",

      "spawn_args": [],
      "exec_override": null,
      "http_probe_ports": [],
      "install_script": null,
      "env": { "BRAVE_API_KEY": "<用户填>" },
      "tool_timeout_secs": null,

      "install_state": "installed",
      "installed_command": "npx",
      "tool_count": 2,
      "install_stdout": "...(截断 4 KB)",
      "install_stderr": "...(截断 4 KB)",
      "last_install_ms": 1762000000000
    },
    "my-private-mcp": { "source": "custom", "kind": "docker" }
  }
}
```

`servers` MUST 为 `IndexMap`——LSP 曾因 `HashMap` 的 SipHash 随机种子导致每次重启列表乱序（`core/acowork-lsp-relay/src/config.rs:50-58` 注释 + `test_lsp_servers_config_iteration_order_is_stable`），MCP 不得重犯。

字段归属：

- **元数据段**（`kind/spec/runner/entry_point/transport/url/description/install_hint/category`）来自 preset，preset 升级时刷新；
- **实例段**（`spawn_args/exec_override/http_probe_ports/install_script/env/tool_timeout_secs`）用户所有，preset 升级不动；`install_script` 在 `source=="custom"` 时为**内联脚本文本**，在 `source=="preset"` 时为 `assets/mcp_install/` 下的**文件名**（继承 preset 元数据段）；
- **状态段**（`install_state/installed_command/tool_count/install_stdout/install_stderr/last_install_ms`）由 install / health_check 写。

`install_state` 取值沿用 ADR-072 `InstallState`：`unknown` / `detected` / `installing` / `installed` / `failed`。

### 5.3 合并规则

```
load_mcp_servers(node_data_dir, preset_registry):
  servers = read({node_data_dir}/mcp_servers.json) or empty
  for (name, preset) in preset_registry.presets:
    if name in servers:
      servers[name].元数据段 ← preset 刷新          # D9
      servers[name].实例段 保持                      # D9
      servers[name].状态段 保持                      # D9
      servers[name].source ← "preset"               # 曾以同名 custom 存在则转正
    else:
      servers[name] = McpServerEntry {
        source: "preset",
        元数据段: preset,
        实例段: preset 默认值（spawn_args / http_probe_ports 拷入；
              env 取 optional_env 默认，required_env 留空）,
        状态段: install_state = "unknown",
      }
  # servers 中 source=="custom" 且不在 preset 里的条目原样保留
  return servers
```

重名策略：preset 名与 custom 名冲突时**以已存在条目为准**（不静默覆盖）；UI 在「新增 custom」时校验名称唯一（沿用 `mcp_catalog_api.rs` 现有 duplicate-name 400 的语义）。

### 5.4 写入与权限（D11）

```
save_mcp_servers(node_data_dir, servers):
  final = {node_data_dir}/mcp_servers.json
  tmp   = final.with_extension("json.tmp")
  write(tmp, serde_json::to_string_pretty(servers))
  f.sync_all()
  set_permissions(tmp, 0o600)          # POSIX；Windows 走 ACL 或降级 no-op + warn
  rename(tmp, final)                   # POSIX 原子
```

`env` 含 API key 明文，故 runtime 文件 MUST 0600。API 响应侧 MUST 掩码——沿用 `mcp_catalog_api.rs::mask_sensitive_env()` 的关键字表（`key` / `token` / `secret` / `password`）与 `has_secrets` 标记，迁至 Node 侧。

### 5.5 API 与反代（D5）

**Node 侧（`:19900`，node token 鉴权）**

```
GET    /api/mcp/servers                       # preset ∪ custom ∪ state
POST   /api/mcp/servers                       # 新增 custom（name 冲突 409）
PUT    /api/mcp/servers/{name}                # 改实例段（env / spawn_args / ...）
DELETE /api/mcp/servers/{name}                # 卸载记录（不删 pipx/cargo 产物）
POST   /api/mcp/servers/{name}/install        # 执行 install + health_check，写回 state
GET    /api/mcp/servers/{name}/install/status # in-flight 阶段（沿用 InstallStage）
GET    /api/mcp/presets                       # 只读 preset registry
```

LSP 同构：`/api/lsp/servers`、`/api/lsp/servers/{name}/install`、`/api/lsp/presets`。

`POST .../install` 入参由 ADR-072 的全量 `McpInstallSpec` **收窄**为 `name`（Node 端查 preset）+ `env`；`source=="custom"` 的条目已自带 spec，install 时不再传 spec。

**Gateway 侧**：`/api/nodes/{node_id}/mcp/{*tail}` → `http://{node.proxy_endpoint}/api/mcp/{*tail}`，注入 `X-ACowork-Node-Token`。与 ADR-089 D2 的 `/api/nodes/{id}/lsp/{*tail}` 同一实现路径（`core/acowork-gateway/src/http/lsp_proxy.rs` + `http/proxy.rs`），仅换前缀。Gateway MUST NOT 缓存 server list。

### 5.6 Runtime 读取路径（D7）

Runtime 新增 `--node-data-dir`（env `ACOWORK_NODE_DATA_DIR`，由 Node spawn 时注入，见 `core/acowork-node/src/process/spawn.rs`）。启动流程：

```
1. 读 {node_data_dir}/mcp_servers.json     → 本机物理可用集合
2. 读 {agent_home}/config/agent_mcp.json   → 本 agent 的 active 子集 + env override
3. active ⊆ available 求交，spawn 之
```

`RuntimeConfigUpdate` 的 catalog 推送路径废弃。`--node-data-dir` 缺失时 MUST 明确报错，MUST NOT 静默降级为空 MCP 列表。

### 5.7 agent 层（D6）

`{agent_home}/config/agent_mcp.json` **保留**，但字段语义变更：

| 字段 | 变更前 | 变更后 |
|------|--------|--------|
| `catalog` | Gateway 推送 | 废弃（改由 §5.6 读本机文件） |
| `local` | `mcp_install` tool 写入 | 废弃（agent 不再安装） |
| `active_server_names` | catalog 子集开关 | **保留**，改为「本机 `mcp_servers.json` 条目名的子集」（ADR-069 工具级 opt-in 语义不变） |

删除 `core/acowork-runtime/src/tools/builtin/mcp_install.rs`、`mcp_uninstall.rs`、`core/acowork-runtime/src/mcp_notify.rs` 的 install 通知路径，以及 `core/acowork-core/src/tools/mod.rs:62` 的 builtin 注册项与 `core/acowork-runtime/src/tools/builtin/mod.rs:282` 的 `"mcp_install"` 条目。

理由：MCP 以 Node 为单位安装，agent 只是消费者；agent 级安装会绕过 Node 的 runtime 文件，制造第三个真相源。

### 5.8 LSP 同构改造（D12）

LSP 补齐两项缺失能力，与 MCP 共用约定：

1. **custom server**：`POST /api/lsp/servers`（`source: "custom"`），schema 沿用 `LspServerEntry`（`candidates` / `args` / `candidate_args` / `install_hint` / `install_script` / `root_markers`）。
2. **install state 持久化**到 `{node_data_dir}/lsp_servers.json`，重启不再全量重探（改为启动一次校验 + 用户显式 Refresh）。

`builtin_lsp_defaults()` 保留为兜底，与 `builtin_mcp_defaults()` 对称。

### 5.9 custom server 的内联脚本执行（D13）

**与 ADR-072 决策 6 的关系**：决策 6 规定 install 接口「拒绝任意 shell 字符串拼接」，只接受结构化 `McpPackageSpec`（kind 白名单）。本 ADR **放宽该约束于 `source=="custom"` 且 `kind=="script"` 的条目**，理由不是便利性，而是**威胁模型并未因此改变**：

- 安装一个 custom MCP server 本身就是执行第三方代码——`npm`/`pypi`/`cargo`/`go` 四种 kind 拉的是任意包，`docker` 拉的是任意镜像。这些 kind 早就等于任意代码执行。
- 因此「允许粘贴脚本」没有新开攻击面，只是把同一风险换成用户看得懂的形式。决策 6 原本防的是**未授权方注入 spec**（API 层拼接），而不是**授权用户主动安装任意软件**。

权限边界因此落在**谁能调这个接口**，不在**接口能收什么**：

| 约束 | 内容 |
|------|------|
| 鉴权 | `POST /api/mcp/servers` 与 `.../install` MUST 经 node token 鉴权（`X-ACowork-Node-Token`），且 Gateway 侧 MUST 校验调用者对该 Node 的权限（ADR-087 Node 归属） |
| 不自动执行 | 内联脚本 MUST NOT 在 `POST /api/mcp/servers`（新增条目）时执行；仅当用户显式点 Install（`POST .../install`）才执行 |
| 执行前展示 | UI MUST 在执行前完整展示将要运行的脚本文本，由用户确认 |
| 超时 | 沿用 LSP 的 idle-timeout：60 秒无 stdout/stderr 输出即杀（`acowork_core::process::run_command_with_idle_timeout`），避免静默挂死 |
| 落盘 | 脚本全文存 `mcp_servers.json` 实例段（0600），Repair 时复用同一文本，不要求用户重粘 |
| 平台 | 单字段足够——runtime 文件是 per-node 的，用户在某 Node 上粘贴适配该 Node OS 的脚本（`NodeInfo.os` / `NodeInfo.arch` 已上报，UI 可显示目标 OS 辅助判断） |

preset 的 `kind=="script"` 仍走 `assets/mcp_install/<name>.{sh,ps1}` 文件名解析，MUST NOT 接受内联文本（preset 文件只读，内容随套件签名分发）。

---

## 6. 对既有 ADR 的修正

| ADR | 位置 | 原文 | 修正 |
|-----|------|------|------|
| ADR-055 | §6.1 职责表 Gateway ③ | 「全局资源权威（providers/**MCP**/search/user profile/embedding 模型库）」 | 移除 MCP → 「全局资源权威（providers/search/user profile/embedding 模型库）」；MCP/LSP server 列表归 Node |
| ADR-055 | §6.1 职责表 Node Agent | 「④ 节点本地 Sidecar 宿主（LSP relay）… ⑥ 节点状态上报」 | 追加「⑦ MCP/LSP server registry 与安装状态权威（`mcp_servers.json` / `lsp_servers.json`）」 |
| ADR-055 | §3 L2 数据布局表 | Gateway 私有 `{data_dir}` 列举 `mcp_catalog` | 移除 `mcp_catalog`；`mcp_catalog.json` 文件本身废弃 |
| ADR-072 | 决策 3（catalog 分层） | `McpServerConfigDef` 增 `install` 字段承载 registry | **取代**：`install` 字段保留用于 wire 兼容，但 registry 权威源改为 `assets/mcp_presets.json` + `{node_data_dir}/mcp_servers.json`；install API 入参收窄为 `name` + `env` |
| ADR-072 | 决策 6（安全边界） | 「只接受 catalog 中已注册条目的 `McpInstallSpec` 或前端传回的结构化 `McpPackageSpec`（kind 白名单校验），拒绝任意 shell 字符串拼接」 | **部分放宽**：`source=="custom"` 且 `kind=="script"` 允许内联脚本文本（§5.9）。约束从「接口能收什么」移到「谁能调」——node token 鉴权 + ADR-087 Node 归属校验 + 执行前 UI 确认 + 不自动执行。preset 的 script kind 不变（仍走只读文件名） |
| ADR-072 | 关联条目 | 「Node Agent——MCP 安装最终可能跨机器分发，本 ADR 的 PackageSpec 与机器无关」 | 本 ADR 兑现该伏笔；`McpPackageSpec` 与命令推导矩阵**不动** |

ADR-055 需加一行修订记录指向本 ADR。

---

## 7. 后果

### 7.1 正面

1. 远程 Node 的 MCP 从「假装有」变为真实可用——修掉原始 bug。
2. 单一真相源，无对账。Gateway 无状态化，跨机部署不再返回 5xx。
3. preset 从前端孤岛升为平台资产：CLI / 第三方 / Node 自检都能读。
4. install state 持久化，重启不重跑分钟级安装。
5. LSP 顺带补齐 custom server 能力。
6. UI 收敛为「选 Node → 单表（preset ∪ custom + status 列）」，两卡片之争消失。

### 7.2 负面与代价

1. **破坏性 API 变更**：`/api/mcp-catalog*` 全删，前端 `mcpStore.ts`（640 行、11 处 fetch）需重写为 Node 维度。
2. **数据迁移**：现存 `{gateway_data_dir}/mcp_catalog.json` 需一次性拆入各 Node 的 `mcp_servers.json`。单机模式下二者同机，迁移可直接 fs 访问；多 Node 部署下旧 catalog 无法判定「当初装在哪台机」，MUST 采取 §8 的保守策略。
3. **Runtime 新增启动参数**：`--node-data-dir` 缺失时报错（§5.6），旧部署需同步更新 spawn 参数。
4. **preset 升级会改写用户文件的元数据段**：`mcp_servers.json` 由程序写，用户手改的元数据会被刷新——需在 UI 或文件头说明「手改仅对实例段有效」。

### 7.3 风险

| 风险 | 缓解 |
|------|------|
| Node 离线时 UI 空列表，用户误判「没装」 | 反代返回明确错误码（`node_offline`），UI 显示「节点不可达」而非空表 |
| 多进程并发写同一文件 | 单 Node 单进程写自身文件；Node 内 install 沿用 ADR-072 决策 7 幂等 + `InstallGuard` 串行 |
| Windows 无 POSIX 原子 rename | 先 `delete` 目标（非原子，存在窗口）或 `MOVEFILE_REPLACE_EXISTING`；失败降级为直接覆盖 + warn |
| 旧 `mcp_catalog.json` 迁移目标 Node 不可判定 | 见 §8：默认迁移到 `local_node_id()` 对应 Node，其余 Node 首次访问为空并提示重装 |

---

## 8. 迁移与回滚

**迁移（一次性，Node 启动时执行）**

```
if exists({gateway_data_dir}/mcp_catalog.json)
   and not exists({node_data_dir}/mcp_servers.json):
    条目按 §5.2 runtime schema 转换
      source: 命中 preset 名 → "preset"，否则 "custom"
      install_state: 原条目存在即标 "installed"
                     （保守假设：能进旧 catalog 说明曾通过 health_check）
    写入 {node_data_dir}/mcp_servers.json (0600)
    原文件改名 mcp_catalog.json.migrated（不删）
```

仅对 `local_node_id()`（Gateway 同机 Node）执行；远程 Node 无旧数据可读，首次启动为空并提示重装。

**回滚**：本 ADR 是纯归属变更。回滚 = 恢复 `mcp_catalog_api.rs` + `mcp_catalog.json` 读写路径；`mcp_servers.json` 保留即无损（新格式是旧格式的超集）。回滚窗口内 MUST NOT 删除 `mcp_catalog.json.migrated`。

---

## 9. 实施边界

按依赖顺序，每步可独立 review：

| # | 内容 | 触及 |
|---|------|------|
| P1 | `assets/mcp_presets.json` + `assets/mcp_install/`（8 个 preset 从 `mcp-presets.ts` 1:1 翻译为 §5.2 preset schema）；`acowork-mcp` 加 `McpPresetRegistry`（候选路径 + `builtin_mcp_defaults()` 兜底） | `assets/`、`core/acowork-mcp/src/` |
| P2 | `acowork-node` 加 `mcp_servers.rs`：runtime schema、§5.3 合并、§5.4 原子写 + 0600、env 掩码 | `core/acowork-node/src/` |
| P3 | Node HTTP API（§5.5 Node 侧 7 端点），install 复用 `acowork_mcp::{probe_runtime, run_install, health_check}` 原样；`kind=="script"` 的 custom 条目走内联脚本，用 `run_command_with_idle_timeout` 执行（§5.9） | `core/acowork-node/src/node_api.rs` |
| P4 | Gateway 反代 `/api/nodes/{id}/mcp/*`（复用 ADR-089 D2 实现）；删 `mcp_catalog_api.rs`；改 `AGENTS.md` 与 ADR-055 §6.1 | `core/acowork-gateway/src/http/` |
| P5 | Runtime 读 `{node_data_dir}/mcp_servers.json`（`--node-data-dir`）；删 `mcp_install` / `mcp_uninstall` / `mcp_notify` install 路径；`agent_mcp.json` 收敛为 `active_server_names` | `core/acowork-runtime/src/`、`core/acowork-node/src/process/spawn.rs` |
| P6 | Desktop：`mcp-presets.ts` 硬编码数组删除、`mcpStore.ts` 改 Node 维度、McpTab 加 Node 选择器 + status 列、custom server 表单加内联脚本粘贴框 + 执行前确认（§5.9） | `apps/acowork-desktop/src/` |
| P7 | 打包：`build.rs` 拷贝 `mcp_presets.json` + `mcp_install/`；`tauri.conf.json` `bundle.resources` 增两项 | `apps/acowork-desktop/src-tauri/` |
| P8 | 迁移器（§8）+ `ci.sh` 红线：Gateway MUST NOT 出现 `mcp_catalog` / `lsp_servers` 读写 | `dev/ci.sh` |
| P9 | LSP 同构（§5.8）：`{node_data_dir}/lsp_servers.json`、custom server、state 持久化 | `core/acowork-lsp-relay/src/`、`core/acowork-node/src/sidecar/` |

P1–P3 不动 Gateway / Runtime / UI，可先落地供 review 合并规则与文件布局。

---

## 10. 开放问题

原三个开放问题已全部定案，无遗留：

| 原问题 | 结论 | 落点 |
|--------|------|------|
| preset 是否允许用户级 override 文件（私有 registry 场景） | **不允许** | D1 |
| `mcp_presets.json` 与 `lsp_servers.json` 是否合并为单一 `assets/servers.json` | **不合并**（schema 差异大，合并只造出 `Option` 字段堆） | D14 |
| custom server 的 install 脚本从哪来 | **设置界面粘贴内联脚本**，per-Node 一份，显式点 Install 才执行 | D13、§5.9 |

