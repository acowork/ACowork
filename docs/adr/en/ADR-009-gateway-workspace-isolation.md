# ADR-009: Gateway Workspace Isolation

## Status
Accepted

## Context

The Gateway has historically accessed agent workspace directories directly — reading and writing files under `{install_path}/workspace/` and `{install_path}/manifest.toml`. This violates the principle that the Gateway should only manage its own `{data_dir}`, while the Runtime owns its workspace.

Five violations were identified:

| ID | File | Operation | Path |
|----|------|-----------|------|
| V1 | `workspaces.rs` | READ + WRITE | `{install_path}/workspace/config/agent_workspaces.json` |
| V2 | `agents.rs` | READ + WRITE | `{install_path}/manifest.toml` |
| V3 | `agents.rs` | READ | `{install_path}/prompts/*.md` |
| V4 | `lifecycle/manager.rs` | WRITE | `{install_path}/workspace/.identity_delivery.json` |
| V5 | `config_api.rs` | DELETE | `{install_path}/workspace/logs/*.log` |

### Key observation: stopped agents have no UI

The desktop app only shows the **Status** tab for stopped agents. Setup, Memory, Chat, and Workspace UI are all hidden when the agent is not running. This means:

- The Gateway never needs to read workspace/config/manifest data for stopped agents (the UI doesn't consume it)
- The Gateway never needs to write workspace data for stopped agents (no user action can trigger it)
- Stopped agents have no Runtime process, so there is no IPC target

### Existing IPC infrastructure

Several IPC message types already exist that can carry the relevant data:

- `WorkspaceContextUpdate` — already used by 3 of 5 workspace handlers
- `RuntimeConfigUpdate.active_tools` — already pushed by `update_agent_config`
- `IdentityDelivery` — defined in protocol but never used
- `LogRotate` — already used for running agent log cleanup

## Decision

**Gateway never reads or writes agent workspace files.** The rule is absolute:

- **Running agents**: All reads and writes go through IPC. The Gateway sends data to the Runtime, which persists to its own workspace.
- **Stopped agents**: API endpoints return "not operable" or empty data. No fallback file access.

This eliminates the need for dual-path code (IPC + file fallback) entirely.

### Migration plan

| Violation | Strategy | Detail |
|-----------|----------|--------|
| V1 | IPC push + Runtime persist | Gateway sends `WorkspaceContextUpdate` with full config; Runtime writes `agent_workspaces.json` itself. Stopped → return empty list. Fix bug: `update_workspace` missing IPC push. |
| V2 | Remove `write_manifest_tools` | `active_tools` persistence already exists in per-agent config (`{data_dir}/agent_configs/{id}.json`). Delete `write_manifest_tools()`. `read_manifest_tools()` remains as install-time discovery fallback only (no write-back). Stopped → return empty tools. |
| V3 | Remove `read_system_prompt` | System prompt is Runtime-internal. Gateway has no business reading it. Stopped → return null. Running → system prompt comes from per-agent config override (`system_prompt_override`). |
| V4 | `AgentHelloResult` delivery | Delete `std::fs::write(.identity_delivery.json)` in `start_agent()`. Add `identity_entries: Vec<IdentityEntry>` to `AgentHelloResult`. Runtime receives identity after IPC handshake and injects into system prompt. |
| V5 | Runtime self-cleanup | Delete Phase 3 (stopped agent log deletion). Runtime cleans its own old logs on startup. Alternatively: accept this as a package-manager-like exception (pre-start cleanup). |

### Exceptions

The following Gateway operations on `install_path` are **explicitly allowed** because they manage the agent installation itself, not the runtime workspace:

- Package manager: install, uninstall, upgrade, clone, publish
- Agent listing: reading `agent.yaml` / `manifest.toml` for metadata (name, version, description) during `list_agents`

These are install-time operations, not runtime data access.

## Consequences

### What becomes easier
- Gateway code is simpler — no workspace path construction, no dual-path logic
- No risk of Gateway/Runtime file races
- Clean ownership model: Gateway owns `{data_dir}`, Runtime owns `{workspace}`
- Stopped agent API handlers become trivial (return empty/not-operable)

### What becomes harder
- V4 requires changing the Runtime initialization sequence (system prompt must be built after IPC handshake, not before)
- Workspace API behavior changes: stopped agents return empty workspace lists (frontend already handles this gracefully — no workspace UI is shown for stopped agents)
- Any future need to show stopped agent config would require persisting it in `{data_dir}` instead

### Compatibility
- No breaking changes for running agents (IPC path already exists for most operations)
- Stopped agent APIs change behavior: return empty/null instead of reading from workspace
- Frontend is already compatible — stopped agents don't show config/workspace/setup UI

## 4. Implementation status at the time

| Violation | Status |
|------|------|
| V1 | cleaned up (the workspace goes entirely through the reverse proxy) |
| V2 | `write_manifest_tools` deleted; `read_manifest_tools` left as an `#[allow(dead_code)]` remnant |
| V3 | `read_system_prompt` left as an `#[allow(dead_code)]` remnant |
| V4 | cleaned up |
| V5 | cleaned up |

**Note**: the "fix" chosen for V2 / V3 (leaving `#[allow(dead_code)]` dead code) was wrong —
dead code is the breeding ground for the next violation. §5 redid it by "just delete it".

---

## 5. Review and revision (after ADR-055, 2026-09)

### 5.1 Why a review was mandatory

```
ADR-009 (accepted)     context: the Gateway and the Runtime shared a process, gRPC
   |                   the Gateway reading runtime files directly "worked, so leave it"
ADR-040 (split process)  the Gateway → the Runtime became MQTT + HTTP
   |                   the file-access code inside the Gateway was never cleaned up
ADR-055 (cross-machine)  the Gateway and the Runtime can be on different machines
   |                   same-address access becomes cross-network filesystem access
Status quo            some violations have escalated from a "code smell" to a hard correctness failure
```

In a single-machine deployment `install_path` happened to be a local path, so the violation
"looked like it worked". After ADR-055 `install_path` is a **Node-local path** (reported by
the Node) and does not exist on the Gateway machine at all — such violations return
**5xx directly**.

### 5.2 Violations found by the review

| ID | Location | Problem | Severity |
|----|------|------|--------|
| V-A | `acowork-gateway/src/http/skills_api.rs` | the Gateway implemented its own "Minimal SKILL.md parser" reading `{install_path}/skills/` | guaranteed to break cross-machine + **two parsers drifting semantically** (already caused a user-visible bug) |
| V-B | `acowork-gateway/src/http/agents.rs` (the avatar read endpoints) | `std::fs::read` on `{install_path}/assets/avatar*` and `manifest.avatar` | guaranteed to break cross-machine |
| V-C | `acowork-gateway/src/http/agents.rs` (workspace / avatar file browsing) | re-checked against ADR-034 §11.2.A items 22a–22h: **already compliant** — everything goes through `http/proxy.rs` and `http/workspaces.rs` reverse proxying | compliant |
| V-D | `acowork-gateway/src/http/agents.rs`: `read_system_prompt` / `read_manifest_tools` / `write_manifest_tools` | `#[allow(dead_code)]` dead code remnants | a latent hazard |

### 5.3 What this revision implements

1. **All skill reads reverse-proxied**: the Runtime gains `GET /agents/{id}/skills`, `/skills/{name}`, `/skills/{name}/history` (`core/acowork-runtime/src/http/skills.rs`, reusing the single parser `crate::skills::parser`); the Gateway deletes its local parser and the three read endpoints and reverse-proxies through `http/proxy.rs`. `POST /skills/import` **stays in the Gateway** — it does not read Runtime-private files, it delegates to the local Node control plane to unpack (ADR-055 §6.2), which was already correct cross-machine.
2. **Avatar reads reverse-proxied**: the Runtime gains `GET /agents/{id}/avatar`, `/avatar-file`, `/manifest/avatar-assets` (`core/acowork-runtime/src/http/avatar.rs`); the Gateway's three read endpoints become reverse proxies. The **write** endpoints (`DELETE /avatar-file`, `POST /manifest/avatar`, `avatar-config`) **stay in the Gateway** — they still involve the Gateway's own avatar cache and publish flow.
3. **The V2 / V3 / V-D dead code is deleted** (no `#[allow(dead_code)]` souvenirs).
4. **The rule itself is made unbypassable**: a lint is added to `dev/ci.sh` (see below); this ADR gets a Chinese version; `AGENTS.md` gains a boundary rule.

### 5.4 The boundary rule after revision

> **Gateway = communication + resource management + reverse proxy.**
> Any read or write of Agent Runtime private data (skills, prompts, conversations, memory, the agent's own embedding, `{install_path}/assets`) may **only** go through Runtime HTTP reverse proxying, and the Gateway process must never touch the filesystem directly.

The only exception is §2.2 (install-time package management + agent list metadata reads).

### 5.5 Not yet handled (tracked separately)

- **Highest priority — residue of the §5.4 redline**: `install_path` is a **Node-reported node-local path**, but three places still treat it as a filesystem root and will certainly break cross-machine:
  - `http/agents.rs` `update_agent_manifest_avatar` (the Publish wizard writing `manifest.toml`)
  - `http/agents.rs` `validate_path_within_install` + `delete_avatar_file` (deleting `{install_path}/assets/*`)

  Note that the **content** of `manifest.toml` is already in the Gateway's memory — the Node's retained `InstalledAgentInfo` message carries the full `manifest_toml` text (`state.rs::upsert_installed_from_node`). So the correct fix is not "read" but to make the **write** take the same path: an MQTT push to the Node → the Node writes it to disk itself, with the Gateway's in-memory `AgentInfo.manifest` and avatar cache synced separately.
- `{install_path}/workspace` is concatenated into a string and stuffed into `RunningAgentInfo.workspace` in 4 places (`mqtt/dispatch.rs` ×3, `http/agents.rs` ×1): a **pure display/logging field that never touches the filesystem**, so it is topologically harmless. But these 4 places are counted by the §5.4 ceiling lint and are false positives, so they are kept as a "a path must never be genuinely used" sentinel.
- The boundary between the embedding sidecar API (provided by the Gateway) and per-agent embedding (internal to the Runtime) lacks a written contract; a short ADR is worth adding.
- `agents/{id}/prompts/reload` (ADR-063 §3.7.6): confirm it is an MQTT push → Runtime reload rather than a direct file write.

## 6. References

- [ADR-055 Remote Runtime Node topology](./ADR-055-remote-runtime-node-topology.md)
- [ADR-058 Workspace filesystem events](./ADR-058-workspace-fs-watcher-mqtt-event.md)
- Review report: [gateway-runtime-isolation-review.md](../../review/zh/gateway-runtime-isolation-review.md)
