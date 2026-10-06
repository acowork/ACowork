# ADR-017: Agent Avatar Runtime Configuration — manifest as Install Default, agent_config.json as Mutable Runtime Config

> **Chinese source of truth**: [ADR-017](../zh/ADR-017-agent-avatar-runtime-config.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft (pending implementation)

## Date

2026-07-10

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

- `core/acowork-core/src/protocol.rs` — `RuntimeConfigUpdate` / `ConfigSnapshot` gain `avatar` / `builtin_avatar`
- `core/acowork-runtime/src/agent_config.rs` — `AgentConfig` gains `avatar` / `builtin_avatar`
- `core/acowork-runtime/src/agent/session/session_manager.rs` — `RuntimeConfigOverrides` gains the avatar fields
- `core/acowork-runtime/src/startup/session_init.rs` — seed the avatar from manifest on first start
- `core/acowork-runtime/src/cli.rs` — persist the new fields on `RuntimeConfigUpdate`
- `core/acowork-runtime/src/grpc/client.rs`, `core/acowork-core/src/proto_bridge.rs` — protobuf bridge
- `core/acowork-gateway/src/http/agents.rs` — new `GET/PUT /api/agents/:id/avatar-config`; effective avatar in `list_agents` / `get_agent_detail`
- `core/acowork-gateway/src/http/agent_config.rs` — DTO gains the avatar fields
- `apps/acowork-desktop/src/components/results/AgentSetupTab.tsx` — two-tab avatar picker
- `apps/acowork-desktop/src/components/common/AgentAvatar.tsx` — render from the effective avatar
- `apps/acowork-desktop/src/lib/avatar.ts` — custom avatar list URL helper
- `apps/acowork-desktop/src/lib/types.ts` — `AgentAvatarAsset` type
- `apps/acowork-desktop/src/stores/agentStore.ts` — avatar refresh logic

---

## Context

### Problem 1 — the frontend can only pick builtin icons

The avatar picker in `AgentSetupTab.tsx` shows only a builtin icon grid and stores the
choice in frontend localStorage as `profile.avatarIconId`. But the agent package
already supports `avatar = "assets/avatar.jpg"` in `manifest.toml` — a preinstalled custom
avatar in the install directory. The frontend never exposes this capability.

### Problem 2 — manifest.toml is treated as mutable runtime config

PublishWizard currently edits `avatar` / `builtin_avatar` directly in `manifest.toml
via `POST /api/agents/:id/manifest/avatar`. That breaks the semantics of
manifest as the "install declaration file" — a user changing an avatar is a
personal / machine-local runtime preference and must not be written back into the
package declaration.

| Problem | Explanation |
|---------|-------------|
| Package declaration is polluted | after a user changes the avatar, the manifest no longer represents the original package |
| Upgrade semantics get muddled | when the package upgrades, does the new manifest overwrite the user change? |
| Publish semantics get muddled | rebuilding / republishing may bake runtime preferences into the package |
| Rollback is hard | cannot tell "author default" from "user-set value" |
| Unclear ownership | Gateway / Runtime are already converging runtime config onto `config/agent_config.json` |

### Problem 3 — no custom avatar list or upload

The user cannot list the custom avatar files already in the install directory (such as
`assets/avatar.jpg`, `assets/avatar-02.jpg`), upload a new one, or switch between
several of them.

## Decision

### Core principles

1. **manifest.toml is only the install declaration and the initial recommended value** — produced by the agent author or the publish flow, never modified afterwards because a user set an avatar
2. **The runtime avatar config is written to `config/agent_config.json`** — the same file as `max_output_tokens`, `max_iterations` and the other runtime settings, persisted by the Runtime
3. **Seed from the manifest at initialization** — when `agent_config.json` is first created, `avatar` / `builtin_avatar` are copied from `manifest.toml`
4. **The frontend avatar picker becomes two tabs** — a Custom tab listing the files in the install directory plus upload, and the existing Builtin icon grid
5. **The Gateway can read and write the avatar config** — the avatar is UI metadata that takes no part in Runtime execution, so the Gateway must be able to read it while the agent is stopped

### Effective avatar resolution order

```
1. config/agent_config.json.avatar          ← custom avatar chosen by the user at runtime
2. config/agent_config.json.builtin_avatar  ← builtin icon chosen by the user at runtime
3. manifest.toml.avatar                     ← install default (fallback)
4. manifest.toml.builtin_avatar             ← install default (fallback)
5. deterministic/random builtin fallback    ← last resort
```

Rule: `avatar` and `builtin_avatar` are mutually exclusive — setting one clears the other.

### Field design

`config/agent_config.json` gains the fields, reusing the manifest field names for
consistency:

```json
{ "max_output_tokens": 32768, "max_iterations": 200,
  "avatar": "assets/avatar-02.jpg", "builtin_avatar": null }
```

or a builtin choice:

```json
{ "avatar": null, "builtin_avatar": "icon-05" }
```

### API design

**`GET /api/agents/:id/avatar-config`** — returns the current effective avatar config.
Does not require the agent to be running.

```json
{ "agent_id": "com.example.agent", "avatar": "assets/avatar-02.jpg",
  "builtin_avatar": null, "source": "config" }
```

`source` reports where the effective value came from: `"config"` | `"manifest"` |
`"fallback"`.

**`PUT /api/agents/:id/avatar-config`** — updates the avatar config. Does not require
the agent to be running. Writes `config/agent_config.json` directly (read-modify-write
merge touching only the avatar fields, preserving everything else).

```json
{ "avatar": "assets/avatar-02.jpg", "builtin_avatar": "" }
```

| Field value | Meaning |
|-------------|---------|
| non-empty string | set to this value |
| `""` | clear the field |
| field absent | leave unchanged |

Rule: setting `avatar` automatically clears `builtin_avatar`, and vice versa.

**`GET /api/agents/:id/manifest/avatar-assets`** — lists the custom avatar files in the
install directory matching the naming pattern.

```json
{ "agent_id": "com.example.agent",
  "assets": [ { "relative_path": "assets/avatar.jpg" },
             { "relative_path": "assets/avatar-02.jpg" } ] }
```

Matching rule: `assets/avatar*.{png,jpg,jpeg,gif,webp,svg}`, sorted with `avatar.*` first,
then `avatar-02.*`, `avatar-03.*`, and so on.

**`GET /api/agents/:id/avatar-file?path=assets/avatar-02.jpg`** — previews an unselected
custom avatar file. Path traversal guard + extension allowlist, returns image bytes.

**Reuse `POST /api/agents/:id/manifest/file?path=...`** — uploads a new custom avatar
into the install directory. Already implemented, no change needed.

**Modify `GET /api/agents` / `GET /api/agents/:id`** — the `avatar` / `builtin_avatar`
returned by `list_agents` and `get_agent_detail` become the effective values (config
first, manifest fallback).

### Initialization strategy

At Runtime startup (`session_init.rs`):

```rust
if agent_config.json 不存在:
    agent_cfg.avatar = manifest.avatar.clone();
    agent_cfg.builtin_avatar = manifest.builtin_avatar.clone();
    save_agent_config(work_dir, &agent_cfg);
else:
    // existing config, do not overwrite. Missing avatar fields fall back to the
    // manifest in the GET API
    load existing config
```

Backwards compatibility: when `agent_config.json` already exists without the avatar
fields, the GET API returns the manifest value as the effective avatar with
`source = "manifest"`. Nothing is auto-written; the values land in the file only after
the user first saves.

### Concurrency safety

Both the Gateway and the Runtime may write `config/agent_config.json`, so every write
must be a read-modify-write merge with an atomic write (tmp + rename):

- Gateway updating the avatar: load → change only `avatar` / `builtin_avatar` → atomic write
- Runtime updating execution config: load → change only the execution fields → atomic write

Each side only touches the fields it owns and preserves the other side's fields.

### Frontend interaction

The avatar area of `AgentSetupTab.tsx` becomes two tabs — a Custom tab listing the files
from `avatar-assets` with click-to-select and the current one highlighted, plus a circular
`+` button that opens the file picker → upload → refresh the list → auto-select the new
file; and a Builtin tab keeping the existing icon grid.

Switching tabs or selecting an item calls `PUT /api/agents/:id/avatar-config`. After a
successful save, clear the avatar blob cache and call `fetchAgents()` to refresh the UI.

---

## Alternatives

### A — write manifest.toml directly (rejected)

The current PublishWizard behavior; see the Context section for the problems.

### B — a separate `config/agent_profile.json`

Splitting the avatar away from the execution config would give a cleaner boundary and
let the Gateway fully own the profile file. But it adds another config file for a
payload of only **2 fields**, and `agent_config.json` is already the aggregation point
for per-agent runtime config.

**Not adopted**: read-modify-write merge is sufficient for concurrency safety at this size.

### C — store the avatar in frontend localStorage only

The current `AgentSetupTab` behavior (`profile.avatarIconId`). Simple, but it does not
sync across devices, does not reflect the package author's default, and stays cut off
from the preinstalled manifest avatar.

**Not adopted**: server-side persistence is required for consistency across devices and
restarts.

---

## Implementation plan

**Phase 1 — data structures.** `AgentConfig` gains `avatar: Option<String>` /
`builtin_avatar: Option<String>` plus a `seed_avatar_from_manifest_if_missing(work_dir, manifest)`
helper; `RuntimeConfigOverrides` gains the same two fields (update `is_empty()`,
`merge()`, and tests); `RuntimeConfigUpdate` and `ConfigSnapshot` gain them; the protobuf
bridge stays in sync.

**Phase 2 — Gateway API.** Add the four new endpoints (`avatar-config` GET/PUT,
`manifest/avatar-assets`, `avatar-file`) in `http/agents.rs`; return the effective avatar
from `list_agents` / `get_agent_detail`; add the fields to the `agent_config.rs` DTO.

**Phase 3 — Runtime init and persistence.** Seed the avatar from the manifest on first
start in `session_init.rs`; persist the new fields when handling `RuntimeConfigUpdate` in
`cli.rs`.

**Phase 4 — frontend.** Add the `AgentAvatarAsset` / `AgentAvatarAssetsResponse` /
`AvatarConfigResponse` types; add the `resolveAgentAvatarFileUrl()` and
`fetchAvatarAssets()` helpers plus the post-save blob cache strategy; replace the inline
builtin picker in `AgentSetupTab.tsx` with the two-tab component; render from the
effective avatar in `AgentAvatar.tsx`; call `fetchAgents()` after saving in `agentStore.ts`.

---

## Rollback

- The new APIs are purely additive and do not affect the existing `/api/agents/:id/config` or `/api/agents/:id/manifest/avatar`
- The new `agent_config.json` fields use `skip_serializing_if = "Option::is_none"`, so older Runtimes ignore the unknown fields
- The new tab can be feature-flagged, falling back to the plain builtin picker
- The PublishWizard manifest-avatar write path stays untouched for now and is migrated separately later

## Open questions

1. Is the `-XX` suffix pattern such as `avatar-02.jpg` acceptable for custom avatar filenames, or should a different pattern (e.g. `avatar-2.jpg`) be used?
2. Is deleting a custom avatar needed?
3. After uploading a new avatar, should it be selected automatically, or only added to the list for the user to click?
