# ADR-090: MCP/LSP Server Install Ownership Moves to the Node — Read-only Presets, Runtime File as the Single Source of Truth

> **Chinese source of truth**: [ADR-090](../zh/ADR-090-mcp-lsp-install-ownership-node.md)

**Status**: Proposed (pending architecture review)
**Date**: 2026-10-09
**Decision makers**: pending review

**Related**:
- [ADR-055](../zh/ADR-055-remote-runtime-node-topology.md) (§6.1 responsibility table row ③, §3 L2 data layout — this ADR **amends** its classification of MCP as a Gateway global resource)
- [ADR-072](../zh/ADR-072-mcp-install-package-spec.md) (declarative `McpPackageSpec` — this ADR **inherits** its Decisions 1/2/4/5/6/7 and **supersedes** Decision 3)
- [ADR-069](../zh/ADR-069-mcp-tool-level-optin.md) (MCP tool-level opt-in — `active_server_names` semantics unchanged)
- [ADR-089](../zh/ADR-089-lsp-relay-reachability-vs-exposure.md) (D2 reverse-proxy routing pattern — this ADR **reuses** it, no re-design)
- [ADR-019](../zh/ADR-019-lsp-relay-standalone-process.md) / [ADR-030](../zh/ADR-030-sidecar-endpoint-dynamic-push.md) (LSP relay process and endpoint push)

---

## 1. Decision Summary

### 1.1 In one sentence

Installing MCP and LSP servers is a **Node-local event** (stdio servers are `fork/exec`'d by the Runtime, so the binary must live on the same machine), therefore the **authoritative source of the server list is the Node, not the Gateway**.
The Gateway degrades to a **pure reverse proxy plus scheduler**: it holds no MCP/LSP server list at all, and the UI always selects a Node first, then reads and writes per Node.
Preset files in the install directory are **read-only** and serve only as initialization data. `{node_data_dir}/mcp_servers.json` and `{node_data_dir}/lsp_servers.json` are the **single runtime source of truth** (preset ∪ custom ∪ install state).

### 1.2 Key decisions

| # | Decision | Content |
|---|----------|---------|
| D1 | Presets read-only, shipped with the suite | `assets/mcp_presets.json` + `assets/mcp_install/`, structurally identical to `assets/lsp_servers.json` + `assets/lsp_install/`. Runtime MUST NOT write these files; **no** user-level preset override file is provided (former Open Question 1 resolved as: not allowed) |
| D2 | Runtime file is the single source of truth | `{node_data_dir}/mcp_servers.json`, `{node_data_dir}/lsp_servers.json` — preset ∪ custom ∪ install state in one file, order preserved via `IndexMap` |
| D3 | Same name, no conflict | The runtime file and the preset file share a name (`lsp_servers.json`); the path domain (install directory vs `{node_data_dir}`) disambiguates. No `_state` suffix is introduced |
| D4 | Flat in the Node, no `data/` subdirectory | Follows the existing `{node_data_dir}` convention (`identity.json`, `state.json`, `packages/`, `logs/`, `runtime-logs/` all flat); the two new JSON files sit at the same level |
| D5 | Gateway holds no server list | Delete `core/acowork-gateway/src/http/mcp_catalog_api.rs`; remove all `/api/mcp-catalog*` routes. UI reads via `/api/nodes/{node_id}/mcp/servers` (reusing ADR-089 D2 reverse proxy) |
| D6 | Agent-level install capability removed | Delete the Runtime built-in `mcp_install` / `mcp_uninstall` tools. Agents **consume** MCP, they do not install it |
| D7 | Runtime reads the local file directly | The Runtime reads `{node_data_dir}/mcp_servers.json` at startup (new `--node-data-dir`) instead of relying on the Gateway `RuntimeConfigUpdate` catalog push |
| D8 | `source` field persisted | Every entry carries `source: "preset" \| "custom"`, used for UI grouping and preset-upgrade decisions |
| D9 | Merge rule: metadata refreshed, instance config and state preserved | A preset upgrade refreshes `kind/spec/description/install_hint/category`; user `env/spawn_args/exec_override` and `install_state` are preserved |
| D10 | No reset-to-preset | No "restore preset defaults" entry point; a user who misconfigures edits it back manually |
| D11 | File mode 0600 + atomic write | Runtime files contain plaintext API keys, so they MUST be `mode 0600`; writes MUST be `tmp + fsync + rename` |
| D12 | LSP refactored identically | LSP gains custom servers and persisted install state (both currently missing), sharing the MCP load/merge/write conventions |
| D13 | Custom-server install script = paste into the settings UI | The instance section gains `install_script` (inline script text, not a filename). The user pastes the script in the MCP/LSP settings UI and the Node executes it with `run_command_with_idle_timeout` (reusing the LSP idle-timeout mechanism). A preset's `install_script` remains a **filename** under `assets/mcp_install/`; the two are disambiguated by `source` |
| D14 | Preset files are not merged | `assets/mcp_presets.json` and `assets/lsp_servers.json` stay as two files (former Open Question 2 resolved as: no merge needed) — the schemas differ too much (MCP has `env`/`required_env`, LSP has `candidates`/`root_markers`), so merging would only produce a field pile of `Option`s |

---

## 2. Background and Current Facts

### 2.1 What MCP install actually does (not "registration only")

Install in `core/acowork-mcp/src/install.rs` does three things: `probe_runtime()` (PATH lookup) → `run_install()` (actually runs `pipx/cargo/go install`, `docker`, or `assets/mcp_install/*.sh`) → `health_check()` (spawns the derived command, initialize → list_tools).

`PackageKind` splits into two classes, which **determines how a remote Node must be handled**:

| kind | `derive_install_command()` | Side-effect location | Remote feasibility |
|------|---------------------------|----------------------|--------------------|
| `npm` / `pypi`+`uvx` | `None` (`npx -y` / `uvx` resolve fresh each spawn) | none (or ephemeral cache) | pushing the spawn config suffices |
| `pypi`+`pipx`/`pip`, `cargo`, `go`, `docker`, `script` | present | `~/.local/pipx/venvs/`, `~/.cargo/bin/`, `~/go/bin/`, local image cache | **must execute on the machine where the spawn happens** |

This is the root cause behind D5/D7: installed artifacts are filesystem facts, and only the Node knows them.

### 2.2 Presets currently exist only in the frontend

The 8 built-in presets are hardcoded in `apps/acowork-desktop/src/lib/mcp-presets.ts` (`playwright`, `computer-use`, `brave-search`, `exa-search`, `context7`, `docling`, `notion`, `figma`; categories browser / search / document / knowledge / design).

Consequences:

1. Zero built-ins on the Rust side — the Gateway sees the full spec only when `POST /api/mcp-catalog/install` arrives, so it cannot probe or validate in advance.
2. Non-Desktop clients (CLI, third parties) have no preset list available.
3. The `assets/` directory contains **no mcp-related files** (currently only `lsp_servers.json`, `lsp_install/`, `offline_providers.json`, `offline_embedding_providers.json`).

### 2.3 The LSP physical chain (the reference implementation for this ADR)

| Stage | Location |
|-------|----------|
| Preset metadata | `assets/lsp_servers.json` (151 lines, `version: 1`, `IndexMap<String, LspServerEntry>`) |
| Install scripts | `assets/lsp_install/{rust,python,typescript,go,clangd,json,yaml,markdown,kotlin,java,css,html}.{sh,ps1}` |
| dev copy | `apps/acowork-desktop/src-tauri/build.rs:142-156` → `target/{profile}/bin/` |
| release packaging | `apps/acowork-desktop/src-tauri/tauri.conf.json:58` `bundle.resources` mapping |
| Locator injection | `apps/acowork-desktop/src-tauri/src/commands/gateway.rs:545` `env("ACOWORK_LSP_CONFIG_DIR", resource_dir)` |
| Load candidates | `core/acowork-lsp-relay/src/config.rs::build_config_candidates()`: `$ACOWORK_LSP_CONFIG_DIR` → `$CARGO_MANIFEST_DIR/../../assets` → `exe_dir` → `cwd` |
| Fallback | `builtin_lsp_defaults()` (`config.rs:239`, **not** `#[cfg(test)]` — it is the safety net when the JSON is missing) |

ADR-072 declared this mechanism as its reference but ported only the **command derivation matrix**, not the **asset layout**.

### 2.4 Two capabilities LSP currently lacks

1. **Cannot add servers** — users can only override existing keys in `lsp_servers.json`, so the "preset ∪ custom merge" problem never surfaced.
2. **Install state is not persisted** — state lives in an in-memory `RwLock<LspServersWithStatus>` and is re-probed on every restart. MCP cannot do the same: `pipx install` takes minutes, and re-running it on every restart is unacceptable.

### 2.5 Current `{node_data_dir}` contents

`core/acowork-node/src/cli.rs:60-62`: `--data-dir` (alias `--work-dir`, env `ACOWORK_NODE_HOME`), default `$HOME/.acowork/acowork-node`. Observed contents:

```
identity.json  state.json  packages/  logs/  runtime-logs/
```

No `data/` subdirectory. D4 chooses flat accordingly.

---

## 3. Alternatives

| Option | Description | Rejection reason |
|--------|-------------|------------------|
| A. Gateway keeps the registry, MQTT dispatches install | Gateway retains `mcp_catalog.json`; install is dispatched over MQTT to the Node for execution | Two sources (Gateway registry + Node reality) will always drift; Gateway state is untrustworthy while the Node is offline; in Desktop single-machine mode both are on the same host, so the extra layer buys nothing |
| B. MCP-as-a-Service (a resident MCP gateway process per Node) | Mirror lsp-relay, MCP over RPC | MCP is a bidirectional long-lived JSON-RPC session (tool call / list_resources / subscribe), not an LSP-style one-shot initialize handshake; splitting into a service requires rewriting the transport layer |
| C. Support remote only for `npm`/`uvx`, pin the rest to the Gateway host | Capability split by kind | Directly contradicts the goal (a Runtime drifting to a remote Node still having working MCP) |
| D. Two-card UI (registry card + per-node installed card) | Split metadata and state across panes | The server list lives on the Node, so only the "Node view" form exists; separate panes create an illusion of a global registry. The LSP single-table-plus-status-column design is already proven sufficient |
| **E. This ADR** | Read-only presets + Node runtime file as sole truth + Gateway as pure reverse proxy | — |

---

## 4. Rationale

1. **Co-locating install and spawn is a hard constraint.** stdio MCP servers are `fork/exec`'d by the Runtime, so the binary must be on the same filesystem. "Installed or not" is therefore an attribute of the Node — not of the Agent, and certainly not of the Gateway.
2. **One source beats two.** Option A needs continuous reconciliation; under this model the Gateway is stateless and every read forwards to the Node, so consistency is structural.
3. **Per-Node filtering in the UI is already a fact.** The MCP/LSP settings panes already filter by Node (one-to-one with the Node), so "the Gateway has a global list" has no consumer in the UI.
4. **The LSP chain is proven.** Asset layout, packaging, candidate paths, and fallback all have working implementations; D1/D3 copy rather than invent.

---

## 5. Detailed Design

### 5.1 File layout

```
install directory (read-only, shipped with the suite)
  assets/mcp_presets.json          # new, 8 built-in presets
  assets/mcp_install/*.sh|*.ps1    # new, script-kind fallback
  assets/lsp_servers.json          # unchanged
  assets/lsp_install/*.sh|*.ps1    # unchanged

{node_data_dir}/                   # $HOME/.acowork/acowork-node (flat per D4)
  identity.json                    # existing
  state.json                       # existing
  mcp_servers.json                 # new, single runtime truth (D2)
  lsp_servers.json                 # new, same name as the preset (D3)
  packages/  logs/  runtime-logs/  # existing
```

Preset lookup follows the LSP candidate order:

```
1. $ACOWORK_MCP_CONFIG_DIR/mcp_presets.json
2. $CARGO_MANIFEST_DIR/../../assets/mcp_presets.json   (dev/test)
3. {exe_dir}/mcp_presets.json                          (release)
4. {cwd}/mcp_presets.json                              (dev convenience)
5. builtin_mcp_defaults()                              (safety net, 1-2 core presets only)
```

Desktop already passes `resource_dir` to the Gateway via `ACOWORK_LSP_CONFIG_DIR`; `mcp_presets.json` lands in the same directory, so P7 reuses that path and MUST NOT add a second env var (unless separation becomes necessary, as a separate case).

### 5.2 Schema

**Preset (read-only, `assets/mcp_presets.json`)** — fields are ADR-072 `McpPackageSpec` plus display fields:

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

**Runtime (read-write, `{node_data_dir}/mcp_servers.json`)** — each entry has three sections:

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
      "env": { "BRAVE_API_KEY": "<user supplied>" },
      "tool_timeout_secs": null,

      "install_state": "installed",
      "installed_command": "npx",
      "tool_count": 2,
      "install_stdout": "...(truncated at 4 KB)",
      "install_stderr": "...(truncated at 4 KB)",
      "last_install_ms": 1762000000000
    },
    "my-private-mcp": { "source": "custom", "kind": "docker" }
  }
}
```

`servers` MUST be an `IndexMap` — LSP once had its list shuffle on every restart because `HashMap`'s SipHash seed is random per process (`core/acowork-lsp-relay/src/config.rs:50-58` comment plus `test_lsp_servers_config_iteration_order_is_stable`). MCP must not repeat that.

Field ownership:

- **Metadata section** (`kind/spec/runner/entry_point/transport/url/description/install_hint/category`) comes from the preset and is refreshed on preset upgrade;
- **Instance section** (`spawn_args/exec_override/http_probe_ports/install_script/env/tool_timeout_secs`) belongs to the user and is never touched by preset upgrades; `install_script` holds **inline script text** when `source=="custom"`, and a **filename** under `assets/mcp_install/` when `source=="preset"` (inherited from the preset metadata section);
- **State section** (`install_state/installed_command/tool_count/install_stdout/install_stderr/last_install_ms`) is written by install / health_check.

`install_state` reuses ADR-072 `InstallState`: `unknown` / `detected` / `installing` / `installed` / `failed`.

### 5.3 Merge rule

```
load_mcp_servers(node_data_dir, preset_registry):
  servers = read({node_data_dir}/mcp_servers.json) or empty
  for (name, preset) in preset_registry.presets:
    if name in servers:
      servers[name].metadata section ← refreshed from preset     # D9
      servers[name].instance section kept                        # D9
      servers[name].state section    kept                        # D9
      servers[name].source ← "preset"       # promoted if it previously existed as a custom of the same name
    else:
      servers[name] = McpServerEntry {
        source: "preset",
        metadata section: preset,
        instance section: preset defaults (spawn_args / http_probe_ports copied in;
                        env seeded from optional_env defaults, required_env left empty),
        state section: install_state = "unknown",
      }
  # entries in servers with source=="custom" and no preset of that name are kept as-is
  return servers
```

Name collision policy: when a preset name collides with an existing custom entry, **the existing entry wins** (no silent overwrite). The UI validates name uniqueness on "add custom" (carrying over the duplicate-name 400 semantics from `mcp_catalog_api.rs`).

### 5.4 Write path and permissions (D11)

```
save_mcp_servers(node_data_dir, servers):
  final = {node_data_dir}/mcp_servers.json
  tmp   = final.with_extension("json.tmp")
  write(tmp, serde_json::to_string_pretty(servers))
  f.sync_all()
  set_permissions(tmp, 0o600)          # POSIX; Windows uses an ACL or degrades to no-op + warn
  rename(tmp, final)                   # atomic on POSIX
```

`env` holds plaintext API keys, so the runtime file MUST be 0600. API responses MUST mask — reusing the keyword table from `mcp_catalog_api.rs::mask_sensitive_env()` (`key` / `token` / `secret` / `password`) and the `has_secrets` flag, moved to the Node side.

### 5.5 API and reverse proxy (D5)

**Node side (`:19900`, node token auth)**

```
GET    /api/mcp/servers                       # preset ∪ custom ∪ state
POST   /api/mcp/servers                       # add custom (409 on name collision)
PUT    /api/mcp/servers/{name}                # edit the instance section (env / spawn_args / ...)
DELETE /api/mcp/servers/{name}                # remove the record (does not delete pipx/cargo artifacts)
POST   /api/mcp/servers/{name}/install        # run install + health_check, write back state
GET    /api/mcp/servers/{name}/install/status # in-flight stage (reuses InstallStage)
GET    /api/mcp/presets                       # read-only preset registry
```

LSP mirrors this: `/api/lsp/servers`, `/api/lsp/servers/{name}/install`, `/api/lsp/presets`.

The `POST .../install` request body **narrows** from ADR-072's full `McpInstallSpec` to `name` (the Node looks up the preset) plus `env`; entries with `source=="custom"` already carry their spec, so install takes no spec for them.

**Gateway side**: `/api/nodes/{node_id}/mcp/{*tail}` → `http://{node.proxy_endpoint}/api/mcp/{*tail}`, injecting `X-ACowork-Node-Token`. Same implementation path as ADR-089 D2's `/api/nodes/{id}/lsp/{*tail}` (`core/acowork-gateway/src/http/lsp_proxy.rs` + `http/proxy.rs`), only the prefix changes. The Gateway MUST NOT cache the server list.

### 5.6 Runtime read path (D7)

The Runtime gains `--node-data-dir` (env `ACOWORK_NODE_DATA_DIR`, injected by the Node at spawn time — see `core/acowork-node/src/process/spawn.rs`). Startup:

```
1. read {node_data_dir}/mcp_servers.json     → physically available set on this machine
2. read {agent_home}/config/agent_mcp.json   → this agent's active subset + env overrides
3. intersect active ⊆ available, then spawn
```

The `RuntimeConfigUpdate` catalog push path is retired. When `--node-data-dir` is missing the Runtime MUST fail loudly and MUST NOT silently degrade to an empty MCP list.

### 5.7 The agent layer (D6)

`{agent_home}/config/agent_mcp.json` is **kept**, but field semantics change:

| Field | Before | After |
|-------|--------|-------|
| `catalog` | pushed by the Gateway | retired (replaced by the local file read in §5.6) |
| `local` | written by the `mcp_install` tool | retired (agents no longer install) |
| `active_server_names` | subset switch over `catalog` | **kept**, redefined as a subset of names in the local `mcp_servers.json` (ADR-069 tool-level opt-in semantics unchanged) |

Delete `core/acowork-runtime/src/tools/builtin/mcp_install.rs`, `mcp_uninstall.rs`, and the install-notification path in `core/acowork-runtime/src/mcp_notify.rs`, along with the built-in registration entry at `core/acowork-core/src/tools/mod.rs:62` and the `"mcp_install"` item at `core/acowork-runtime/src/tools/builtin/mod.rs:282`.

Rationale: MCP is installed per Node and agents merely consume it. Agent-level installation would bypass the Node's runtime file and create a third source of truth.

### 5.8 LSP refactored identically (D12)

LSP gains the two missing capabilities, sharing the conventions with MCP:

1. **Custom servers**: `POST /api/lsp/servers` (`source: "custom"`), schema reusing `LspServerEntry` (`candidates` / `args` / `candidate_args` / `install_hint` / `install_script` / `root_markers`).
2. **Persisted install state** in `{node_data_dir}/lsp_servers.json`; restarts no longer re-probe everything (one verification at startup plus an explicit user Refresh).

`builtin_lsp_defaults()` remains as the safety net, symmetric with `builtin_mcp_defaults()`.

### 5.9 Inline Script Execution for Custom Servers (D13)

**Relation to ADR-072 Decision 6**: Decision 6 states the install endpoint "rejects arbitrary shell string concatenation" and accepts only a structured `McpPackageSpec` (kind allowlist). This ADR **relaxes that constraint for entries with `source=="custom"` and `kind=="script"`**. The justification is not convenience but the fact that **the threat model does not change**:

- Installing a custom MCP server is itself executing third-party code — the `npm`/`pypi`/`cargo`/`go` kinds pull arbitrary packages and `docker` pulls arbitrary images. Those kinds have always amounted to arbitrary code execution.
- Allowing a pasted script therefore opens no new attack surface; it restates the same risk in a form the user can read. What Decision 6 actually guarded against was an **unauthorized party injecting a spec** (string concatenation at the API layer), not an **authorized user deliberately installing arbitrary software**.

The security boundary therefore moves to **who may call the endpoint**, not **what the endpoint accepts**:

| Constraint | Content |
|------------|---------|
| Authentication | `POST /api/mcp/servers` and `.../install` MUST require node token auth (`X-ACowork-Node-Token`), and the Gateway MUST verify the caller's permission over that Node (ADR-087 Node ownership) |
| No automatic execution | An inline script MUST NOT execute on `POST /api/mcp/servers` (adding the entry); it runs only when the user explicitly clicks Install (`POST .../install`) |
| Show before running | The UI MUST display the full script text for confirmation before execution |
| Timeout | Reuses the LSP idle-timeout: killed after 60 seconds without stdout/stderr output (`acowork_core::process::run_command_with_idle_timeout`), so a silent hang cannot stall the install |
| Persistence | The full script text is stored in the `mcp_servers.json` instance section (0600); Repair reuses the same text rather than asking the user to paste again |
| Platform | One field suffices — the runtime file is per-Node, so the user pastes a script suited to that Node's OS (`NodeInfo.os` / `NodeInfo.arch` are already reported and the UI can display the target OS) |

A preset with `kind=="script"` still resolves through `assets/mcp_install/<name>.{sh,ps1}` filenames and MUST NOT accept inline text (preset files are read-only and their contents ship signed with the suite).

---

## 6. Amendments to Existing ADRs

| ADR | Location | Original text | Amendment |
|-----|----------|---------------|-----------|
| ADR-055 | §6.1 responsibility table, Gateway ③ | "global resource authority (providers / **MCP** / search / user profile / embedding model library)" | MCP removed → "global resource authority (providers / search / user profile / embedding model library)"; MCP/LSP server lists belong to the Node |
| ADR-055 | §6.1 responsibility table, Node Agent | "④ node-local Sidecar host (LSP relay) … ⑥ node status reporting" | Append "⑦ authority for the MCP/LSP server registry and install state (`mcp_servers.json` / `lsp_servers.json`)" |
| ADR-055 | §3 L2 data layout table | Gateway-private `{data_dir}` lists `mcp_catalog` | Remove `mcp_catalog`; the `mcp_catalog.json` file itself is retired |
| ADR-072 | Decision 3 (catalog layering) | `McpServerConfigDef` gains an `install` field carrying the registry | **Superseded**: the `install` field stays for wire compatibility, but registry authority moves to `assets/mcp_presets.json` + `{node_data_dir}/mcp_servers.json`; the install API request narrows to `name` + `env` |
| ADR-072 | Decision 6 (security boundary) | "accepts only the `McpInstallSpec` of an entry already registered in the catalog, or a structured `McpPackageSpec` returned by the frontend (kind allowlist); rejects arbitrary shell string concatenation" | **Partially relaxed**: `source=="custom"` with `kind=="script"` now accepts inline script text (§5.9). The constraint moves from "what the endpoint accepts" to "who may call it" — node token auth + ADR-087 Node ownership check + UI confirmation before execution + no automatic execution. The preset script kind is unchanged (still a read-only filename) |
| ADR-072 | Related section | "Node Agent — MCP install may eventually be distributed across machines; this ADR's PackageSpec is machine-independent" | This ADR cashes that promise; `McpPackageSpec` and the command derivation matrix are **untouched** |

ADR-055 needs a revision-log line pointing here.

---

## 7. Consequences

### 7.1 Upside

1. MCP on remote Nodes goes from "pretending to exist" to actually working — the original bug is fixed.
2. Single source of truth, no reconciliation. The Gateway becomes stateless here, so cross-machine deployments no longer return 5xx.
3. Presets are promoted from a frontend island to a platform asset: CLI, third parties, and Node self-checks can all read them.
4. Install state persists; restarts no longer re-run minute-long installs.
5. LSP picks up custom-server support along the way.
6. The UI collapses to "pick a Node → one table (preset ∪ custom + status column)"; the two-card-versus-one-card debate disappears.

### 7.2 Downside and cost

1. **Breaking API change**: all `/api/mcp-catalog*` routes are deleted, and the frontend `mcpStore.ts` (640 lines, 11 fetch sites) must be rewritten around the Node dimension.
2. **Data migration**: the existing `{gateway_data_dir}/mcp_catalog.json` must be split once into each Node's `mcp_servers.json`. In single-machine mode both paths are on the same host and the migration is a plain filesystem operation; in multi-Node deployments the old catalog cannot say which machine it was installed on, so the conservative strategy in §8 MUST apply.
3. **New Runtime startup parameter**: `--node-data-dir` fails loudly when absent (§5.6), so existing deployments must update their spawn arguments in step.
4. **Preset upgrades rewrite the metadata section of a user-owned file**: `mcp_servers.json` is written by the program, so hand-edited metadata gets refreshed — the UI or a file header note MUST state that manual edits only hold for the instance section.

### 7.3 Risks

| Risk | Mitigation |
|------|------------|
| UI shows an empty list while the Node is offline, and users read it as "not installed" | The reverse proxy returns an explicit error code (`node_offline`); the UI shows "node unreachable" rather than an empty table |
| Concurrent writes to the same file | One Node, one process writing its own file; installs inside a Node reuse ADR-072 Decision 7 idempotency plus `InstallGuard` serialization |
| Windows lacks POSIX atomic rename | Delete the target first (non-atomic, a window exists) or use `MOVEFILE_REPLACE_EXISTING`; on failure degrade to direct overwrite plus warn |
| The target Node for migrating the old `mcp_catalog.json` is undecidable | See §8: default to the Node for `local_node_id()`; other Nodes start empty and prompt for reinstall |

---

## 8. Migration and Rollback

**Migration (one-time, at Node startup)**

```
if exists({gateway_data_dir}/mcp_catalog.json)
   and not exists({node_data_dir}/mcp_servers.json):
    convert entries to the §5.2 runtime schema
      source: name matches a preset → "preset", otherwise "custom"
      install_state: any pre-existing entry is marked "installed"
                     (conservative assumption: reaching the old catalog
                      means it once passed health_check)
    write {node_data_dir}/mcp_servers.json (0600)
    rename the original to mcp_catalog.json.migrated (do not delete)
```

This runs only for `local_node_id()` (the Node co-located with the Gateway). Remote Nodes have no legacy data to read, so they start empty and prompt for a reinstall.

**Rollback**: this ADR is a pure ownership change. Rolling back means restoring the `mcp_catalog_api.rs` + `mcp_catalog.json` read/write path; keeping `mcp_servers.json` is harmless (the new format is a superset of the old). Within the rollback window `mcp_catalog.json.migrated` MUST NOT be deleted.

---

## 9. Implementation Scope

In dependency order; each step is independently reviewable:

| # | Content | Touches |
|---|---------|---------|
| P1 | `assets/mcp_presets.json` + `assets/mcp_install/` (the 8 presets translated 1:1 from `mcp-presets.ts` into the §5.2 preset schema); `acowork-mcp` gains `McpPresetRegistry` (candidate paths + `builtin_mcp_defaults()` safety net) | `assets/`, `core/acowork-mcp/src/` |
| P2 | `acowork-node` gains `mcp_servers.rs`: runtime schema, §5.3 merge, §5.4 atomic write + 0600, env masking | `core/acowork-node/src/` |
| P3 | Node HTTP API (the 7 Node-side endpoints in §5.5); install reuses `acowork_mcp::{probe_runtime, run_install, health_check}` as-is; custom entries with `kind=="script"` run their inline script via `run_command_with_idle_timeout` (§5.9) | `core/acowork-node/src/node_api.rs` |
| P4 | Gateway reverse proxy `/api/nodes/{id}/mcp/*` (reusing the ADR-089 D2 implementation); delete `mcp_catalog_api.rs`; update `AGENTS.md` and ADR-055 §6.1 | `core/acowork-gateway/src/http/` |
| P5 | Runtime reads `{node_data_dir}/mcp_servers.json` (`--node-data-dir`); delete the `mcp_install` / `mcp_uninstall` / `mcp_notify` install paths; `agent_mcp.json` narrows to `active_server_names` | `core/acowork-runtime/src/`, `core/acowork-node/src/process/spawn.rs` |
| P6 | Desktop: delete the hardcoded array in `mcp-presets.ts`, rewrite `mcpStore.ts` around the Node dimension, add a Node selector and status column to McpTab, and add an inline-script paste box with pre-execution confirmation to the custom-server form (§5.9) | `apps/acowork-desktop/src/` |
| P7 | Packaging: `build.rs` copies `mcp_presets.json` + `mcp_install/`; `tauri.conf.json` `bundle.resources` gains two entries | `apps/acowork-desktop/src-tauri/` |
| P8 | Migrator (§8) + a `ci.sh` red line: the Gateway MUST NOT read or write `mcp_catalog` / `lsp_servers` | `dev/ci.sh` |
| P9 | LSP refactor (§5.8): `{node_data_dir}/lsp_servers.json`, custom servers, persisted state | `core/acowork-lsp-relay/src/`, `core/acowork-node/src/sidecar/` |

P1–P3 touch neither the Gateway, the Runtime, nor the UI, so they can land first for review of the merge rule and file layout.

---

## 10. Open Questions

All three former open questions are now settled; none remain:

| Original question | Resolution | Where |
|-------------------|------------|-------|
| Should user-level overrides of the preset file be allowed (private registry scenarios)? | **Not allowed** | D1 |
| Should `mcp_presets.json` and `lsp_servers.json` merge into one `assets/servers.json`? | **No merge** — the schemas differ too much; merging would only produce a pile of `Option` fields | D14 |
| Where does a custom server's install script come from? | **Pasted inline in the settings UI**, one copy per Node, executed only on an explicit Install click | D13, §5.9 |

