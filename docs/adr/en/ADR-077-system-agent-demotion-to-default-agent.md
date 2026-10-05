# ADR-077: Demoting the System Agent to a Preinstalled Default Agent — Removing the Gateway's Privileged Builtin Path

> **Chinese source of truth**: [ADR-077](../zh/ADR-077-system-agent-demotion-to-default-agent.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Decided (finalized 2026-11-12)

## Date

2026-09-13 (finalized 2026-11-12)

## Decision Makers

大鱼 (Dayu)

## Predecessors

- [ADR-055](../zh/ADR-055-remote-runtime-node-topology.md) (Node topology — this document
  revises its §6.2 re: the System Agent path)
- [ADR-059](../zh/ADR-059-parallel-onboarding-handshake.md) (Bootstrap handshake — this
  document revises §4.3 / §6.1 / §7.5 / §12.2 / §15.1, which positioned the System Agent as a
  Required subsystem)
- [ADR-075](../zh/ADR-075-node-identity-uuid-and-node-name.md) (Node identity UUID — this
  document revises its D6 description of the `"local"` placeholder boundary)
- [ADR-073](../zh/ADR-073-agent-instance-identity-decomposition.md) (Agent instance identity —
  this ADR is consistent with it: the System Agent's instance identity is no different from an
  ordinary agent's)

---

## 1. Decision Summary

### 1.1 In one sentence

**`com.acowork.system` is demoted from "a privileged builtin Runtime managed directly by the
Gateway + a Required capability of BootstrapState" to "a preinstalled default ordinary Agent
distributed alongside the Gateway binary": the Gateway no longer auto-installs / auto-starts it,
no longer holds its runtime state, no longer prevents it from being uninstalled, and no longer
treats it as a Required capability of BootstrapState. It remains preinstalled (bundled with the
Gateway) and still provides capabilities through the standard Intent protocol (`identity:query` /
`identity:observe`); all other install / start / distribution / crash-recovery behavior is on
equal footing with Calendar / Search / senior-engineer-agent.**

### 1.2 The background principle

**The Gateway's responsibility boundary = communication + resource management + reverse proxy**
(see [AGENTS.md](../../../AGENTS.md) and [ADR-009 §5](../zh/ADR-009-gateway-workspace-isolation.md)).
The System Agent carries "semantic storage and validation of user identity / preferences" — a
**business logic** concern. Making it a privileged builtin component inside the Gateway process
means the Gateway depends on a business chain in reverse: identity semantic validation is
completed via an LLM round-trip, auto-start depends on the Node's retained inventory aggregation,
and BootstrapState ready depends on its readiness. This directly conflicts with the
responsibility boundary principle.

The System Agent's **functionality** (identity / preference) is **kept**; what is removed is its
**carrier form and its privileges**:

- it is no longer part of the Gateway process's startup chain (no longer blocks BootstrapState);
- it is no longer auto-installed / auto-started by Gateway special-case code;
- there is no longer a "cannot be uninstalled" constraint;
- like any ordinary agent, wherever it is installed, its `installed_agents.node_id` is that
  Node's UUID.

### 1.3 The semantic decisions

| # | Decision | Conclusion |
|---|---|---|
| D1 | The System Agent's role | **A bundled default ordinary Agent.** Same class as senior-engineer-agent / document-manager-agent; the only difference is that it ships with the Gateway binary (bundled) rather than being downloaded by the user from a remote store |
| D2 | Gateway auto-install / auto-start | **Delete entirely.** The whole auto-start task in [gateway/mod.rs](../../../core/acowork-gateway/src/gateway/mod.rs) — wait for retained inventory → query the install table → fall back to bundled install → wait another 30s → fetch instance_id / node_id → send start → parse the NodeEvent ack — is removed. The System Agent's first install is performed by **onboarding** (the desktop onboarding wizard, or a manual install) |
| D3 | BootstrapState Required capability | **Delete.** `system_agent` is no longer registered as a Required subsystem. `BootstrapState` no longer waits for the System Agent to reach READY. [ADR-059](../zh/ADR-059-parallel-onboarding-handshake.md) §4.3 / §6.1 / §7.5 / §12.2 / §15.1 are revised in sync |
| D4 | The "cannot be uninstalled" constraint | **Delete.** The "System Agent cannot be uninstalled" guard in the desktop `agentStore` is removed; in the UI the System Agent is consistent with an ordinary agent — installable and uninstallable |
| D5 | `installed_agents.node_id` | **No longer writes the literal `"local"`.** Like an ordinary agent, it records the UUID of the Node hosting it |
| D6 | The `SYSTEM_AGENT_ID` constant | **Kept.** It is still the `agent_id` string of that agent package (`com.acowork.system`) and is still needed for Intent routing / package identification. What is removed is the Gateway's *privileged handling of* `SYSTEM_AGENT_ID`, not the constant itself |
| D7 | Identity / preference data storage | **Out of scope here.** Identity / preference data is provided to the agent by the Gateway over HTTP / MQTT at open / create session time; that design is merged with **ADR-076 (multi-user)**. This ADR only performs the demotion; the System Agent keeps its `memory_recall` / `memory_store` tools and the `identity:query` / `identity:observe` Intent protocol, and the data-source switch lands with ADR-076 |
| D8 | `bundled` distribution | **Kept.** The System Agent still ships with the Gateway binary (`examples/system-agent`); only "whether it is installed automatically the first time" changes from "Gateway-forced" to "onboarding / user decision" |
| D9 | The "Gateway-managed Runtime" semantics | **Converge.** ADR-075 D6's description of "`\"local\"` = the placeholder for a Gateway-managed agent" becomes void once the category "a .agent Runtime managed directly by the Gateway" disappears. The `"local"` remaining in production code narrows to a **bookkeeping sentinel meaning "host Node unknown / this machine"** (`RunningAgentInfo.node_id` fallbacks, the `fs_browse` argument sentinel). It is **not** an "in-process Gateway service placeholder" — no in-process service call chain identified by `"local"` exists in production code. Exact locations and semantics in §3.4 |
| D10 | Compatibility | **Not preserved.** The project is not live. Old records in `[gateway_data_dir]` with `node_id == "local"` and `agent_id == com.acowork.system` in `installed_agents` / `running_agents` are deleted on startup detection; onboarding then reinstalls it onto the local Node UUID |

## 2. Background

### 2.1 The current state (after ADR-059)

The System Agent is a link in the Gateway process's startup chain:

```text
Gateway startup
  → start the local Node
  → wait for the local Node's retained installed_agents (up to 10s)
  → query the install table for com.acowork.system
      → absent: dispatch bundled install → wait another 30s for retained
  → fetch instance_id + node_id
  → send the Node control start command
  → wait for the NodeEvent ack
  → aggregate BootstrapState: system_agent = Required, phase flips to READY once ready
```

It is also written into BootstrapState's Required subsystem set (`registry.register("system_agent",
Required).mark_ready()` when `${SYSTEM_AGENT_ID}` aggregates successfully, in
[dispatch.rs](../../../core/acowork-gateway/src/mqtt/dispatch.rs)).

### 2.2 Three concrete misalignments

**(a) Responsibility overreach**: the Gateway's job is communication / resource management /
reverse proxy. The System Agent is the carrier of identity business logic. Just to start one
identity business agent, the Gateway must poll the Node's retained inventory, maintain a bundled
fallback install, wait out a 30s timeout, and parse a NodeEvent ack — all of which is "business
agent lifecycle management", not the Gateway's job.

**(b) Startup coupling**: BootstrapState reaching READY depends on the System Agent being ready.
Any startup delay of an identity business agent (slow Node, slow install, slow LLM provider)
blocks the entire platform from reaching READY, leaving the desktop's main chat area unusable. A
business agent must not be a precondition for platform readiness.

**(c) The privilege of not being uninstallable**: `agentStore` blocks uninstalling the System
Agent, `dispatch.rs` special-cases `SYSTEM_AGENT_ID`, and ADR-059 lists it as Required — a
"business agent" holding privileges beyond every other agent. That runs opposite to the
direction of ADR-073 ("all agents are equal, instance identities are consistent").

### 2.3 Goals

The **only** two differences between the System Agent and an ordinary agent should be:

1. **bundled**: it ships with the Gateway binary (because it is the platform's recommended
   default agent);
2. **recommended by default**: onboarding suggests installing it (because identity / preference
   is what most users need) — but it is not forced, does not block, and can be uninstalled.

Beyond that, install / start / distribution / crash recovery / BootstrapState participation are
all identical to an ordinary agent.

## 3. Detailed Design

### 3.1 Removing the Gateway's privileged path

| Location | Current | After |
|---|---|---|
| [gateway/mod.rs](../../../core/acowork-gateway/src/gateway/mod.rs) auto-start task | ~200 lines: wait for inventory → bundled install fallback → wait 30s → send start → wait for ack | **Delete.** The System Agent is no longer started by the Gateway |
| [gateway/mod.rs](../../../core/acowork-gateway/src/gateway/mod.rs) `dispatch_bundled_agent_install` | `node_id = LOCAL_NODE_ID` supplied for the System Agent call | keep the function (other bundled call sites still use it), **delete the System Agent call site**; the internal `node_id` semantics follow the remaining callers |
| [mqtt/dispatch.rs](../../../core/acowork-gateway/src/mqtt/dispatch.rs) `SYSTEM_AGENT_ID` special case | `registry.register("system_agent", Required).mark_ready()` when aggregating the installed inventory | **Delete** that branch; installed-inventory aggregation treats the System Agent exactly like every other agent |
| [gateway/mod.rs](../../../core/acowork-gateway/src/gateway/mod.rs) capability comment | lists `system_agent` as a required subsystem | delete that line; the required-subsystem set no longer contains `system_agent` |

### 3.2 BootstrapState participation

The System Agent is removed from the **Required subsystems**. It could be an **Optional**
subsystem (to still drive a desktop status refresh on `version` increments), or it could not be
registered at all (the desktop gets the System Agent's status through the ordinary
`/api/agents` list).

**Chosen: do not register at all.** The System Agent's readiness is expressed by its own Runtime
publishing the retained `acowork/agents/{agent_id}/ready`; the desktop consumes it through the
ordinary agent status path and it does not enter BootstrapState. Rationale: BootstrapState
aggregates "whether the platform can safely bootstrap", and a business agent being ready is not
part of that semantics (§5.4 OCP: do not expose business subsystems).

ADR-059 is revised in sync: §4.3 scenario matrix, §6.1 dependency DAG (delete the `SYS_PREPARE` /
`SYS_INSTALL` nodes and the `system_agent` edges), §7.5, §12.2.3, §14, §15.1.

### 3.3 Frontend

| Location | Change |
|---|---|
| `apps/acowork-desktop/src/stores/agentStore.ts` | delete the "System Agent cannot be uninstalled" guard |
| `apps/acowork-desktop/src/stores/agentStore.ts` | "default-select the System Agent after onboarding" becomes non-forced (with no explicit default, pick the first item in the list) |
| `apps/acowork-desktop/src/components/onboarding/OnboardingFlow.tsx` | the System Agent still appears in onboarding's suggested list (checkable), but is no longer "must install / must start" |
| `apps/acowork-desktop/src/components/layout/SplashScreen.tsx` | no longer polls System Agent readiness |
| `apps/acowork-desktop/src-tauri/src/commands/gateway.rs` | remove the `dependency: SYSTEM_AGENT_ID` BootstrapState declaration |

### 3.4 Narrowing the `"local"` placeholder boundary (revises ADR-075 D6)

ADR-075 D6 originally defined `"local"` as "the placeholder for a Gateway-managed agent". After
the System Agent moves out, the category "a .agent Runtime managed directly by the Gateway" no
longer exists (D9) and that definition becomes void.

**The `node_id: "local"` that actually remain in production code** (audited file by file;
`#[cfg(test)]` fixtures excluded):

| Location | Semantics |
|---|---|
| [http/agents.rs](../../../core/acowork-gateway/src/http/agents.rs) `track_running_agent` | a `RunningAgentInfo.node_id` fallback — the install record has not been aggregated yet and the **hosting Node is unknown** |
| [mqtt/dispatch.rs](../../../core/acowork-gateway/src/mqtt/dispatch.rs) `handle_plaintext_message` / `track_running_agent_for_status` / `reconcile_running_agents` | same — three `RunningAgentInfo` fallbacks |
| [http/fs_browse.rs](../../../core/acowork-gateway/src/http/fs_browse.rs) `browse_fs` | the `?target=` argument sentinel: empty / `local` = browse the Gateway's own machine. **Comparison only** — never published as an MQTT topic |

The narrowed semantics are a **bookkeeping sentinel meaning "host Node unknown / this machine"**;
it does not represent any Runtime's host.

It is **not** an "in-process Gateway service placeholder": no in-process service call chain
identified by `"local"` exists in production code. The `node_id: "local"` occurrences in
[http/doc_proxy.rs](../../../core/acowork-gateway/src/http/doc_proxy.rs) /
[http/embedding_api.rs](../../../core/acowork-gateway/src/http/embedding_api.rs) /
[http/pm_proxy.rs](../../../core/acowork-gateway/src/http/pm_proxy.rs) /
[http/proxy.rs](../../../core/acowork-gateway/src/http/proxy.rs) /
[intent/router.rs](../../../core/acowork-gateway/src/intent/router.rs) are **all in test
fixtures**. An early draft of this ADR mistakenly listed those fixtures as production code;
implementing from that list would find no matching locations.

Non-`node_id` uses of the same string must be distinguished separately: the `"local"` in
[http/provider_api.rs](../../../core/acowork-gateway/src/http/provider_api.rs) is an API key
placeholder value and the `"local"` in
[http/models_api.rs](../../../core/acowork-gateway/src/http/models_api.rs) is a JSON field name —
both unrelated.

ADR-075 D6 and the `AgentInfo.node_id` comment in
[gateway/state.rs](../../../core/acowork-gateway/src/gateway/state.rs) are updated in sync (the
latter already changed to "records the UUID of the hosting Node" when ADR-075 was implemented).

### 3.5 Data compatibility

**No historical paths are preserved.** The Gateway scans `installed_agents` / `running_agents` on
startup:

- a record with `node_id == "local"` and `agent_id == "com.acowork.system"` → delete (a leftover
  of the old privileged path);
- the System Agent is reinstalled by onboarding onto the local Node UUID.

Rationale matches ADR-075 D10: the project is not live, so no migration code is needed.

Boundary: this ADR does not change the System Agent's `agent_id`, `manifest.toml` content,
prompts or skills. The `system = true` marker in `manifest.toml` is kept (descriptive metadata
identifying "bundled with the Gateway", no longer a Gateway privilege switch) or deleted (if
nothing consumes it) — to be confirmed at implementation time.

## 4. Impact List

### 4.1 Rust (`core/acowork-gateway`)

| File | Change |
|---|---|
| `gateway/mod.rs` | delete the System Agent auto-start task (~200 lines); delete `use SYSTEM_AGENT_ID` (if now unused); update the capability comment |
| `mqtt/dispatch.rs` | delete the `SYSTEM_AGENT_ID` → `registry.register("system_agent", Required)` special-case branch; update the test section |
| `http/agents.rs` | the list sort `sort_pins_system_agent_first`: keep as a UX preference (with a comment) or delete (no special case); tighten the `LOCAL_NODE_ID` fallback comment in `resolve_agent_node_id` per §3.4 |
| `gateway/state.rs` | update the `AgentInfo.node_id` comment per §3.4; keep `pub const SYSTEM_AGENT_ID` |
| `http/bootstrap_api.rs` | remove `system_agent` from the Required set in the test fixtures |

### 4.2 Frontend (`apps/acowork-desktop/src`)

| File | Change |
|---|---|
| `stores/agentStore.ts` | delete the non-uninstallable guard; adjust the onboarding default-selection logic |
| `components/onboarding/OnboardingFlow.tsx` | the System Agent goes from "must" to "recommended" |
| `components/layout/SplashScreen.tsx` | remove the System Agent readiness polling |
| `src-tauri/src/commands/gateway.rs` | remove the `SYSTEM_AGENT_ID` BootstrapState dependency |

### 4.3 Documentation / tests

| File | Change |
|---|---|
| `docs/adr/zh/ADR-059-parallel-onboarding-handshake.md` | §4.3 / §6.1 / §7.5 / §12.2.3 / §14 / §15.1 updated in sync |
| `docs/adr/zh/ADR-055-remote-runtime-node-topology.md` | the §6.2 System Agent path description updated |
| `docs/adr/zh/ADR-075-node-identity-uuid-and-node-name.md` | the D6 `"local"` placeholder boundary description updated |
| `core/acowork-gateway/tests/bootstrap_integration.rs` | the System Agent Required tests are deleted / changed to "not required" |
| `core/acowork-gateway/tests/node_ready_e2e.rs` | audit the System Agent readiness assertions |
| `dev/e2e_frontend_smoke/onboarding_installs_all_agents.py` | `if SYSTEM_AGENT_ID not in installed: fail` becomes "if present, verify the instance_id is a UUID" — no longer a failure condition |
| `dev/e2e_frontend_smoke/smoke_test.py` | System Agent readiness is no longer a failure condition |
| `e2e-frontend-smoke-test.md` | updated in sync |
| `docs/design/{zh,en}/02-agent-package.md`, `18-user-identity-simplified.md`, `06-communication.md` | bundled agent / identity path descriptions updated; identity storage points at ADR-076 |
| `examples/system-agent/prompts/system.md` | a header note that after ADR-077 it is an ordinary agent and the identity data source will be provided by the Gateway (ADR-076) |

### 4.4 Unchanged

| Item | Note |
|---|---|
| `examples/system-agent/manifest.toml` | untouched apart from the `system = true` marker decision |
| `prompts/system.md` / `skills/` / `tools` | untouched |
| the `memory_recall` / `memory_store` tools | untouched |
| `IdentityRead` / `IdentityWrite` in `core/acowork-core/src/permission.rs` | untouched (permission definitions are decoupled from the agent implementation) |
| `intent/privacy.rs` | untouched |

> **Implementation revision (§8.1 / §8.2)**: all the `examples/system-agent/*` items listed in
> this section were deleted wholesale during implementation, and the `manifest.system` field was
> removed together with its install-lane mechanism.

## 5. Benefits

1. **Responsibilities return to their proper place**: the Gateway is back within the
   communication / resource management / reverse proxy boundary and no longer embeds an identity
   business chain
2. **Startup decoupling**: BootstrapState reaching READY no longer depends on any business
   agent, shortening the cold-start critical path (both the 10s and the 30s waits are deleted)
3. **Code reduction**: ~200 lines of Gateway auto-start special cases + one BootstrapState
   Required edge + the desktop's non-uninstallable guard + 4 test fixtures are all deleted
4. **Agent equality**: the System Agent's instance identity, lifecycle and uninstallability match
   every other agent (aligned with ADR-073)
5. **A clean `"local"` boundary**: the mixed category "a .agent Runtime managed directly by the
   Gateway" no longer exists

## 6. Open Questions (finalized before implementation)

| # | Question | Decision |
|---|---|---|
| Q1 | Keep or delete the `system = true` marker in `manifest.toml` | **Delete.** Apart from being the semantic anchor for the Gateway's privileged path, the field has no consumer; once the System Agent's privilege is removed it becomes ownerless metadata, so it is simply removed |
| Q2 | Keep or delete `sort_pins_system_agent_first` (list sorting) | **Delete.** The System Agent uses the same list sort as an ordinary agent (by `installed_at` / `name`) and no longer enjoys a privileged display position |
| Q3 | Is the System Agent still registered as an Optional subsystem | **No.** Its readiness is expressed by its own Runtime's retained `acowork/agents/{agent_id}/ready`; the desktop consumes it through the ordinary agent status path and it does not enter BootstrapState |
| Q4 | Does onboarding pre-check the System Agent for installation | **Pre-checked by default** (most users need identity memory) but cancelable; "if the user uninstalled it last time, do not auto-check" must be persisted |

> **Implementation revision (§8.2 / §8.3)**: Q1's deletion scope expanded to the entire
> `InstallPriority` lane mechanism (including the proto field and the clone guard); Q4 was rolled
> back together with the bundled package's deletion — onboarding no longer lists the System
> Agent.

## 7. Implementation Order

1. Delete the Gateway auto-start task (`gateway/mod.rs`)
2. Delete the `system_agent` Required registration branch in `dispatch.rs`
3. Frontend: delete the non-uninstallable guard + adjust onboarding + remove the SplashScreen
   polling
4. Tighten the `"local"` placeholder comments per D9 (`state.rs` / `agents.rs` / `dispatch.rs`)
5. Clean up stale `node_id == "local"` System Agent records on startup
6. Sync the ADR-059 / ADR-055 / ADR-075 reference points
7. Update the e2e / bootstrap tests
8. `cargo test` + `cargo clippy -- -D warnings` + the e2e smoke

---

## 8. Implementation Record (2026-11-12, `feature/adr077`)

At implementation time, following the directive "the project is in development, there is no
compatibility burden, clean up harder", the deletion scope exceeded this ADR's original
boundary. The following **revises** §1 / §4 / §6 / §7, and this section takes precedence.

### 8.1 The System Agent package leaves the repository entirely (revises §1.1 "still bundled")

`examples/system-agent/` (manifest + prompts + skills + assets) and the build artifact
`examples/agent-packages/com.acowork.system.agent` are deleted, and `com.acowork.system` no
longer exists in the codebase in any form. "It is still bundled (ships with the Gateway)" in
§1.1 is void: the Gateway no longer bundles any agent package, and the bundled directory holds
only the 7 example packages at the same level as ordinary agents.

Consequential deletions:

| Location | Deleted |
|---|---|
| `src-tauri/src/commands/agent.rs` | the `"system-agent"` branch in the bundled package name mapping |
| `src-tauri/src/commands/gateway.rs` | the `ensure_system_agent` Tauri command (~167 lines) + the `SYSTEM_AGENT_ID` constant |
| `gateway/state.rs` | the `SYSTEM_AGENT_ID` constant + `find_instance_by_agent_id` (used only by the auto-start task, so it has zero consumers after deletion) |

The `examples/system-agent/*` items in §4.4 "Unchanged" therefore no longer exist.

### 8.2 The `manifest.system` field and the whole install lane mechanism are deleted (exceeds the §4.1 boundary)

Q1 only decided to "delete the `system = true` line". At implementation time the entire mechanism
behind that field was deleted as well: once its sole declarer was gone, `InstallPriority::System`
had no producers, and although the scheduler is strictly FIFO in real deployments it still
carried a sort key, a wire flag and a clone guard.

| Layer | Deleted |
|---|---|
| `acowork-core/src/manifest.rs` | the `pub system: bool` field |
| `acowork-core/src/install/mod.rs` | the `InstallPriority` enum, `from_system_flag`, the `priority` field on the request / ticket, the `is_system` parameter of two constructors |
| `acowork-core/src/install/scheduler.rs` | the dequeue key `(priority, seq)` → pure `seq` (FIFO) |
| `acowork-core/proto/mqtt_payload.proto` | `NodeInstall.system`; field 6 is held by `reserved` to prevent the field number being reused with different semantics in the future |
| `acowork-gateway/src/mqtt/node_control.rs` | the `NodeInstallDispatch.system` field and its `to_proto` passing |
| `acowork-node/src/package/clone.rs` | the "a system agent cannot be cloned" guard (the lane flag's only reader outside the scheduler) |
| 7 × `examples/*/manifest.toml` | the `system = false` lines |

### 8.3 Q4 rolled back

Q4's "pre-check by default in onboarding + do not pre-check if uninstalled last time" was rolled
back together with the package deletion: there is no bundled System Agent to pre-check,
`RECOMMENDED_AGENTS` does not contain the entry, and the `acowork.onboarding.skipSystemAgent`
localStorage key plus its read/write code are all removed.

### 8.4 §7 step 5 skipped

"Clean up stale `node_id == "local"` System Agent records on startup" was not implemented —
after the auto-start task was deleted no code path can create privileged records, and every
remaining `"local"` is a legitimate local bookkeeping sentinel.

### 8.5 e2e scripts

| Script | Change |
|---|---|
| `dev/e2e_frontend_smoke/smoke_test.py` | the TC-BOOT-02 case (System Agent auto-readiness) is deleted: the startup chain no longer auto-starts any agent, so the behavior under test no longer exists. The recovery suite's `latest-session` probe switches to `GET /api/agents` — that suite uses a clean temporary home and installs no agents |
| `dev/e2e_frontend_smoke/onboarding_installs_all_agents.py` | the `N+1` inventory invariant (N user packages + 1 auto-installed system) becomes `N` |
| `dev/e2e_stop_test.ps1` | the default `-AgentId` becomes `com.acowork.senior-engineer` |

### 8.6 Remaining documentation sync

`docs/design/{zh,en}/02-agent-package.md` (delete the `system` field description),
`docs/design/zh/10-debug-protocol.md` (delete the clone restriction),
`docs/prd/{zh,en}/prd.md` and `prd-ui-ux.md`, `docs/adr/zh/ADR-059`, `docs/adr/zh/ADR-075`,
`assets/architecture.svg`.
