# ADR-085: The Agent Lifecycle State Machine (`ready: bool` → the `AgentStatus.state` enum)

> **Chinese source of truth**: [ADR-085](../zh/ADR-085-agent-lifecycle-state-machine.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

**Status**: Accepted (v2 revision, implemented on 2026-10-01 and passed review — lifecycle merged into `AgentStatus`, re-stamp on reconnect, Gateway status-code pass-through, removal of the Runtime's up-front session creation; all three open questions in §10 have been decided; the three implementation-time decisions are recorded in §11)
**Date**: 2026-09-30
**Decision Makers**: Architecture review

**Related**:
- [ADR-033](./ADR-033-mqtt-replace-grpc-websocket.md) (the original source of the `ready` topic — this ADR supersedes its plain-text bool payload)
- [ADR-038](./ADR-038-session-lifecycle-explicit-model.md) (making the session lifecycle explicit — this ADR is its mirror one layer up)
- [ADR-055](./ADR-055-remote-runtime-node-topology.md) (Node-hosted Runtime; the source of the `running_agents` table and the 503 proxy window)
- [ADR-058](./ADR-058-workspace-fs-watcher-mqtt-event.md) (§3.4 makes explicit that the Desktop does **not subscribe** to the `agents/+/ready` topic — this ADR closes that gap by reusing the `status` subscription path)
- [ADR-065](./ADR-065-unify-mqtt-client-lifecycle.md) (unified MQTT client lifecycle; sleep/wake `force_reconnect` — the source of the constraint that a reconnect must not regress state)
- [ADR-073](./ADR-073-agent-instance-identity-decomposition.md) (instance identity; the `status` topic path carries `instance_id`)
- [ADR-076](./ADR-076-multi-user-account-system.md) (multi-user; the source of the two conflated semantics of `/latest-session` 404)
- [ADR-082](./ADR-082-memory-storage-sqlite-vector-fts.md) (session-meta lands in SQLite; the data source for `/sessions`)

---

## 1. Decision Summary

### 1.1 In one sentence

**Deprecate the plain-text `bool` payload of the `agents/+/ready` topic; merge the structured `AgentLifecycleState` enum into the existing `acowork/agents/{instance_id}/status` topic (adding `state` / `detail` fields to `AgentStatus`, no new topic)**, and have the Runtime publish the authoritative state at **every capability boundary** of its startup process. The Runtime's session-class interfaces return `503 session_not_ready` (rather than `404`) before the subsystems are ready, and the Gateway proxy layer **passes through** the Runtime status codes (no longer collapsing non-2xx into 404), so that "the backend is not ready" and "there is genuinely no data" are **distinguishable across the entire chain at the protocol level**. No `ready` field is kept, and no compatibility code is written.

The key insight: **this is not just "adding a field"; it is acknowledging that the existing `ready` was never actually defined**. Both ends misread it — the Runtime emitted `true` having only half finished, and the Desktop treated it as "I can read sessions now" — so within the startup window the `404` ("no session") was misread as fact, and the frontend filled in an orphan session.

The second key insight of the v2 revision: **the state carrier must share a topic with the LWT**. An MQTT connection can register only one Last Will (currently on the `status` topic, `client.rs:986-994`). If lifecycle became its own topic, on a crash the LWT would only fire on `status`, and the retained lifecycle state would **stay stuck at `sessions_ready` forever** — the Desktop would judge "everything allowed" against a dead process, an error of the same shape this ADR aims to eliminate. Merging into `AgentStatus` means the will payload directly carries `state=OFFLINE`, so retained state never goes stale.

### 1.2 Key Decision Table (detailed rationale in §4)

| # | Decision | Conclusion |
|---|----------|-----------|
| 1 | State carrier | **Merge into the existing `acowork/agents/{instance_id}/status` topic** (retained + LWT); add a `state` (enum) and `detail` (string) field to `AgentStatus` — **zero new topics, zero new encoders** (reusing `encode_agent_status_payload`). The independent lifecycle topic is rejected (§5 option G) |
| 2 | State values | `OFFLINE` / `STARTING` / `HTTP_READY` / `SESSIONS_READY` / `FAILED` (five values + `UNSPECIFIED`, see §4.1) |
| 3 | Naming principle | **Name by "what you can do", not "which step you're on"**. No implementation-bound names like `loading_a` / `loading_b` (§4.2) |
| 4 | The `ready` field | **Deprecated**. `AgentListResponse.ready` / `AgentDetailResponse.ready` / `RunningAgentInfo.ready` are all removed; the 6 Desktop consumer sites all switch to reading `state`. No derived field is kept |
| 5 | Compatibility | **No compatibility code**. The `ready` topic is deleted outright; Runtime / Gateway / Desktop must be deployed at the same version (§4.6) |
| 6 | The session interfaces | The Runtime's `/sessions`, `/sessions/latest`, `/sessions/{sid}/config`, etc. return **`503 {"error":"session_not_ready"}`** while the late-bind slots are unfilled; `/sessions/latest` is allowed to return 404 only after the subsystem is ready, and the 404 body is unified to `{"error":"no_session"}` (**including the "exists but not readable" case** — consistent with ADR-076 §decision 4's non-distinction principle, avoiding leaking "this agent has a session" to a non-owner) |
| 7 | Reading `latest_session` | Remove the current "an unfinished background scan also counts as 404": when the scan is unfinished, return `503 session_not_ready` — **do not guess** |
| 8 | The Gateway proxy | `send_runtime_json` **passes through** the Runtime status code and body; collapsing non-2xx into 404 is forbidden; when an endpoint is unregistered, return `503 {"error":"agent_not_running"}` (rather than the current 404). See §4.4b |
| 9 | Desktop subscription | **No new subscription needed**: the `status` snapshot is already forwarded by the Desktop's `agent-event` channel (the `AgentStatusSnapshot` in `workspaceFsEvents.ts`), and the `state` field arrives for free with `AgentStatus`. This removes the "can only see ready via 30s polling" latency (the ADR-058 §3.4 residual gap) |
| 10 | Frontend decision | When `state < SESSIONS_READY`, **prohibit** calling any session read/create interface; the sole trigger for `createSession` is "`SESSIONS_READY` + the account genuinely has zero sessions" (§4.5) |
| 11 | Failure state | A new `FAILED` carrying `detail` (a human-readable reason). `degraded_reasons` moves from "only into `/health`" to "into `AgentStatus.detail`" (§4.4) |
| 12 | Reconnect semantics | **A reconnect must not regress state**: `STARTING` is published once, only on the process's first connection; `run_bootstrap` Step 7 re-stamps the **current value** on reconnect (inheriting the existing `ready_ever` mechanism, see §4.3b) |
| 13 | The initial session | **Remove the Runtime's up-front creation at startup** (the else branch at `session_init.rs:966-995`). Zero sessions is a legal state (`404 no_session`); session creation converges to a single writer: only the Desktop triggers it via `POST /sessions` after `SESSIONS_READY` (§4.7) |

### 1.3 Invariants (must hold)

1. **State is authoritatively declared by the backend; the frontend must not infer state from timeouts / retry counts**. The frontend's capability gating reads only `AgentStatus.state`.
2. **"Not ready" and "none" must be distinguishable at the protocol level**, and that distinction must **run across the whole chain** — a 503 returned by the Runtime must not be translated into a 404 by the Gateway proxy layer (§4.4b). No "not found" may be directly mapped to "genuinely absent".
3. **`state` advances monotonically**: `OFFLINE → STARTING → HTTP_READY → SESSIONS_READY`; `FAILED` may be entered from any non-`OFFLINE` state (a crash returns to `OFFLINE`). **Prohibited**: `SESSIONS_READY → HTTP_READY` regression. **An MQTT reconnect (sleep/wake, a Gateway restart) is not a process restart and must not regress to `STARTING`** — re-stamp the current value on reconnect (§4.3b). Regression is allowed only on a process restart (a new `instance_id`).
4. **State changes must go over a retained topic, and must share a topic with the LWT**. During the failure window, HTTP may simply be unreachable (the Gateway proxy 503s); the frontend **cannot** rely on HTTP to discover "what state I'm in"; on a crash the broker-relayed will must be able to stamp the state back to `OFFLINE`, and retained state must not go stale.
5. **After deprecating `ready`, no residual semantics are left**. No `ready` derived field, no compatibility branch for a "ready but sessions not yet ready" intermediate state.
6. **Consistency of `online` and `state`**: `state != OFFLINE` implies `online == true`; the LWT and the Gateway stop path both set `online=false, state=OFFLINE`. The consumer's defensive rule: when `online == false`, treat as `OFFLINE` regardless of `state`.
7. **"Ensure a session exists" has exactly one writer** (the Desktop, via `POST /sessions`). The Runtime startup path must not create a session (§4.7).

### 1.4 Startup State vs. Capability Comparison (the full view of the failure window)

| `state` | Process | MQTT | HTTP port | `GET /api/agents/{id}/workspaces` | `GET /api/agents/{id}/sessions` | `GET .../latest-session` | Desktop allowed behaviour |
|---|---|---|---|---|---|---|---|
| `OFFLINE` | none/exited | down | — | 503 `agent_not_running` | 503 `agent_not_running` | 503 `agent_not_running` | show "not started", no business calls |
| `STARTING` | up | not/just connected | not registered | 503 `agent_not_running` | 503 `agent_not_running` | 503 `agent_not_running` | show "starting", wait for the `status` push |
| `HTTP_READY` | up | connected | registered | **200** | **503 `session_not_ready`** | **503 `session_not_ready`** | may read workspaces/config; **reading sessions is forbidden** |
| `SESSIONS_READY` | up | connected | registered | 200 | **200** | **200** or `404 no_session` | everything allowed; `createSession` only on `404 no_session` |
| `FAILED` | up/exited | down or up | depends | 503 | 503 | 503 | show `detail`, no business calls |
| `UNSPECIFIED` | — | — | — | gate as `OFFLINE` | gate as `OFFLINE` | gate as `OFFLINE` | UI shows "state unknown (version mismatch?)" distinctly (§10 Q2) |

Contrast with today: the whole `HTTP_READY` row **does not exist** in the current system — when `ready=true`, the session interfaces return an ambiguous 404, so the frontend mistakenly creates a session.

---

## 2. Background and Problem

### 2.1 The trigger: a deleted untitled session resurrects after a restart

On 2026-09-30 the user deleted the untitled sessions of the ponytail / software-architect / senior-engineer agents; after a restart they reappeared. Each of the three agents' SQLite held one orphan session with 0 messages and no title:

```
com.acowork.ponytail          -> 20260930_163229_5e1096 (0 msgs)
com.acowork.software-architect -> 20260930_163237_4cde50, 20260930_163027_d99b41
com.acowork.senior-engineer    -> 20260930_163223_3f69ff
```

**The deletion itself succeeded**: `delete()` in `session_meta.rs` synchronously runs `DELETE FROM sessions` + `DELETE FROM fts_sessions`, and the log's `SessionManager: deleted session` went through the full flow. The problem is not in persistence.

### 2.2 Root cause: `ready`'s semantics are undefined, and the implementation predates its meaning

`publish_ready(true)` has **exactly one call site in the entire codebase** (`agent_init.rs:582`), at the end of Phase A. After Phase B fills the `session_metadata` / `session_config` slots, **no code updates the state again**.

Measured timing of ponytail's 2026-09-30 16:32:28 startup (`workspace/logs/20260930_163228.log`):

```
16:32:28.618  agent_init.rs:582  Phase A ready signal published; Phase B/C continue in background
16:32:29.023  session_init.rs:277  Background session scan complete count=20
16:32:29.058  session_init.rs:968  Initial session created initial_session_id=20260930_162321_353bef
                                       ↑ a 440ms vacuum
```

After `ready=true` was announced there is a **440ms** in which the session subsystem does not exist at all.

### 2.3 The failure chain (hop by hop, each hop with log evidence)

| # | Time | Event | Evidence |
|---|---|---|---|
| 1 | 28.618 | Runtime sends `ready=true` | `20260930_163228.log:32` |
| 2 | 28.655 | Gateway-side `GET /latest-session` → **404** (the Runtime has not registered its HTTP endpoint yet; the request **never reaches the Runtime**) | `20260930_163201.log` |
| 3 | 28.669 | `GET /workspaces` → **503** (same window) | same |
| 4 | 28.771 | `GET /sessions?page=1` → **503** | same |
| 5 | 29.845 | `POST /sessions` → **201**, the orphan session is born | same |
| 6 | 29.936 | Runtime `SessionManager: created new session 20260930_163229_5e1096` | `20260930_163228.log:191` |

The step-2 404 is especially bad: it comes from the **Gateway-side no-endpoint fallback** and is **indistinguishable in the response** from "the agent genuinely has no session". The exact producing path of that 404 needs to be re-verified at implementation time — `proxy_to_runtime`'s main path returns 503 when the endpoint is missing (`proxy.rs:2518-2530`), whereas `send_runtime_json` returns **404** under the same condition (`proxy.rs:2676-2681`), and the node fallback path also has a 404 (`proxy.rs:1112-1120`). Regardless of the concrete source, it exposes the same class of defect: **Gateway maps "backend unavailable" to 404** and conflates it with "no data".

### 2.4 Structural judgement: no patch can fix it

The existing frontend logic:

```ts
const latest = await get().fetchLatestSession(id);   // 404 → null
let target = latest?.session_id ?? null;
if (!target) {
    await get().fetchSessions(id);                     // 503 → catch → sessions = []
    target = get().agents[id]?.sessions[0]?.session_id ?? null;
    if (!target) { await get().createSession(id); return; }   // ← created an orphan
}
```

Three defects stacked; no single-point patch can stop it:

1. The 404 from `/latest-session` mixes the "not ready" and "none" semantics;
2. `fetchSessions`'s catch **translates a 503 into an empty list** (`agentStore.ts:870-876`), misjudging "the interface is not up" as "this account has not a single session";
3. `fetchSessionReqId` is a **globally shared** monotonic counter (`agentStore.ts:215`), so one agent's slow request invalidates another agent's response, further widening the misjudgment window.

**Per invariants 1 and 2, none of these can be fixed by frontend retry counts or timeouts** — the protocol layer cannot express "not ready", so the client can only guess.

### 2.5 Gateway comment and implementation disagree (secondary finding, but confirming the nature of the problem)

The comment at `dispatch.rs:394-397` claims the `ready` topic is published "after Phase A–C have all populated the HTTP server's late-bind slots". The implementation (`agent_init.rs:582`) publishes at the end of Phase A, while Phase B/C continue in the background. **The comment describes a stronger contract than the implementation** — this shows `ready`'s true meaning was never clarified, and docs and code each say their own thing.

### 2.6 Gateway `send_runtime_json` collapses all non-2xx into 404 (new v2 finding)

`proxy.rs:2701-2707`: any Gateway handler going through `fetch_runtime_json` / `send_runtime_json` (e.g. `/api/agents/{id}/conversations/latest`, `chat.rs:58`) **turns any non-success status the Runtime returns into `ApiError::not_found`**; an unregistered endpoint also returns 404 (`proxy.rs:2676-2681`). This means: even if the Runtime, per this ADR, honestly returns `503 session_not_ready`, what reaches the frontend through this path is still a 404 — **invariant 2 would be re-broken at the Gateway layer**. This defect must be fixed in sync with D4 (§4.4b); otherwise the value of the state machine is eaten the moment it leaves the Gateway.

### 2.7 The Runtime creates the initial session up front at startup (new v2 finding)

`session_init.rs:966-995`: when there is no recoverable session (**which is exactly the "the user deleted all sessions" scenario**), the Runtime itself calls `create_session()` in the startup path, building a 0-message, untitled, ownerless Private session. Consequences:

1. a **second writer** for "ensure a session exists" appears, conflicting with decision 10 (the Desktop's `createSession` is the sole trigger);
2. the core regression assertion in §8.2 ("delete all sessions → restart → 0 untitled") is broken by the Runtime itself, regardless of how well the frontend is fixed;
3. every "zero-session restart" accumulates one ownerless garbage session in SQLite.

Its reason for existing (the verbatim comment: "Without this … the frontend ChatPanel stays blank") is precisely the old frontend contract this ADR deprecates. It must be removed too (§4.7).

---

## 3. Goals

1. The frontend **needs no timing assumptions, retry counts, or timeouts** to correctly judge "can I read sessions now".
2. After deleting a session and restarting, **no orphan session reappears** (including a Runtime-created ownerless session).
3. State semantics are unique and authoritative in the backend (eliminating the §2.5 comment/implementation split), and **retained state is not stale after a crash** (LWT on the same topic).
4. Startup failure is **visible** to the user, instead of staying stuck at "starting" forever.
5. The "not ready" vs "none" distinction **runs across the whole Runtime → Gateway → Desktop chain** (eliminating the §2.6 collapse).
6. A convergent blast radius: no second boolean patch, no compatibility branch, no new topic and no new subscription.


## 4. Decisions

### D1: The state carrier and values — merged into `AgentStatus`

**No new topic**. Extend the existing `acowork/agents/{instance_id}/status` (retained + LWT) `AgentStatus`:

```protobuf
enum AgentLifecycleState {
  AGENT_LIFECYCLE_STATE_UNSPECIFIED    = 0;  // unknown / receiving an unrecognized value → gate conservatively as OFFLINE, distinct UI display (§10 Q2)
  AGENT_LIFECYCLE_STATE_OFFLINE        = 1;
  AGENT_LIFECYCLE_STATE_STARTING       = 2;
  AGENT_LIFECYCLE_STATE_HTTP_READY     = 3;
  AGENT_LIFECYCLE_STATE_SESSIONS_READY = 4;
  AGENT_LIFECYCLE_STATE_FAILED         = 5;
}

message AgentStatus {
  string agent_id   = 1;
  bool   online     = 2;
  reserved 3;                      // was sleeping, never reused
  string instance_id = 4;          // ADR-073
  string node_id     = 5;          // ADR-073
  AgentLifecycleState state = 6;   // added by this ADR: capability progress
  string detail = 7;               // added by this ADR: non-empty only on FAILED, the human-readable failure reason
}
```

The reason for merging (rather than an independent lifecycle topic) — **LWT uniqueness is decisive**:

1. An MQTT connection can register **only one Last Will**, currently on the `status` topic (`client.rs:986-994`, will payload = `AgentStatus{online=false}`). After merging, on a crash the will directly carries `state=OFFLINE`, so retained state never goes stale; with an independent topic, after a crash the retained lifecycle would **stay stuck at `SESSIONS_READY` forever**, and the Desktop would judge "everything allowed" against a dead process.
2. The Gateway's stop path is already a second writer of the `status` topic (`agents.rs:1947-1961`, actively publishing the `AgentStatus{online=false}` envelope). After merging, stop → `OFFLINE` comes for free; with an independent topic that path would also have to emit lifecycle, adding a writer and a race.
3. On the Desktop, the `status` snapshot is already forwarded via the `agent-event` channel (the `AgentStatusSnapshot` in `workspaceFsEvents.ts`); after merging **the Desktop adds zero subscriptions**; an independent topic would need a new `agents/+/lifecycle` subscription path.
4. The sole argument in the v1 draft against merging ("the LWT payload carries unneeded fields") costs **two fields, a few dozen bytes** — disproportionate to the correctness benefit above.
5. Encoder reuse: extending `encode_agent_status_payload` (`client.rs:50`) with two parameters is enough — zero new encoders, zero new parsing, zero new test surface.

`online` is kept as the **connection-layer fact** (driven by LWT / stop); `state` is the **capability-layer fact** (driven by startup phases); consistency is constrained by invariant 6.

**The `ready` topic is deleted outright**, no parallel publishing (invariant 5). The `{id}` in the topic path is always the **`instance_id`** (ADR-073, consistent with the existing `status` / `ready` topics); it must not be mistakenly written as the package id at implementation time.

### D2: Naming principle — by capability, not by phase

The user proposed phase names like `loading_a / loading_b / loading_... / ready`. **The "make it an enum" direction is adopted; the "phase naming" form is rejected**, for:

- A state name **binds to the implementation**. Phases A/B/C/D are today's startup flow; tomorrow someone merges B/C, or inserts a phase between `HTTP_READY` and `SESSIONS_READY`, and every frontend `case 'loading_b'` has to change too.
- Yet the question the frontend actually answers is "**can I call the session interfaces now**" — which is **independent of** how many steps the backend takes.
- Capability-named states **remain valid** after a backend refactor: once `loading_b` loses its meaning there is no substitute, whereas `HTTP_READY` is always valid (the fact that HTTP is listening is itself a stable meaning).

`STARTING` is the only value carrying a startup flavour, but it is necessary: it represents the **stable, long-lived** window of "MQTT just connected, HTTP not yet registered" (the `Gateway proxy 503` is exactly the external manifestation of this state), not an implementation detail that can be refactored away.

### D3: Publish state at every capability boundary

| Publication timing | State | Location |
|---|---|---|
| **First** MQTT connection established (once in the process lifetime) | `STARTING` | `client.rs` `run_bootstrap` (see D3b's reconnect constraint) |
| End of Phase A (HTTP listening + `http_endpoint` published) | `HTTP_READY` | `agent_init.rs:582` (**replaces** the existing `publish_ready(true)`) |
| **End of Phase B** (`session_metadata` / `session_config` slots filled) | `SESSIONS_READY` | `session_init.rs` (**new** — today it is a dumb silent point, exactly the failure window. Note: it no longer takes the initial session creation as a precondition, see D7) |
| Any phase fails | `FAILED` + `detail` | the corresponding phase |
| Graceful process exit / Gateway stop path | `OFFLINE` (`online=false, state=OFFLINE`) | `publish_status` / `agents.rs:1947` |
| Process crash | `OFFLINE` | **LWT** (the will payload carries `state=OFFLINE`, relayed by the broker) |

The publication timing of `SESSIONS_READY` has a **must-be-exact-after** constraint: it must be published only **after both** `session_init.rs:847` (filling the `session_metadata` slot) **and** `:879` (filling the `session_config` slot) are complete. Otherwise it repeats "announced ready, but actually not ready".

### D3b: Reconnect semantics — re-stamp the current value, no regression (new in v2)

`run_bootstrap` also runs on a **reconnect of the same process** (ADR-065 sleep/wake's `force_reconnect`; a reconnect after a Gateway restart loses all retained state on the in-memory broker). The existing code already maintains a `ready_ever` atomic flag for this (`client.rs:524-535`): Step 7 re-stamps the retained `ready=true` on reconnect instead of re-sending the initial value.

This ADR inherits and strengthens that mechanism:

- `BootstrapData.ready_ever: AtomicBool` → **`current_lifecycle: AtomicU8`** (holding the newest in-process state);
- `publish_lifecycle(state)` builds in a **monotonicity guard**: reject and warn when the target state is lower than the current value (`FAILED` excepted — it may be entered from any non-`OFFLINE` state; a process restart = a new `instance_id`, which naturally resets);
- `run_bootstrap` Step 7, on reconnect, **re-stamps the current value of `current_lifecycle`** alongside `publish_status(online=true)`;
- `STARTING` is published only when the state is uninitialized (first connection).

Otherwise: a single Gateway restart would regress every already-`SESSIONS_READY` agent back to `STARTING`, and the Desktop would re-gate the session interfaces — violating invariant 3.

### D4: Honest returns from the Runtime session interfaces

`get_latest_session` (`server.rs:1033`) currently **does not check the late-bind slots** and unconditionally returns 404 — meaning even if D3 is implemented perfectly, when there is a gap in state propagation the frontend still reads the 404 as "no session". The protocol layer must protect itself:

| Condition | Response |
|---|---|
| Slot unfilled / scan unfinished | `503 {"error":"session_not_ready"}` + `Retry-After: 2` |
| Ready, a session exists | `200` |
| Ready, genuinely no session | `404 {"error":"no_session"}` |
| Exists but not readable (ADR-076 private session) | `404 {"error":"no_session"}` (**the body is byte-identical to the previous row** — deliberately not distinguished, to avoid leaking "this agent has a session" to a non-owner; see ADR-076 §decision 4) |

`GET /sessions`'s bare 503 when the slot is unfilled (`server.rs:1015`) is likewise changed to `{"error":"session_not_ready"}`, so the frontend need not guess via `Retry-After`.


**Distinguish the two sources of 503**: the Gateway-side 503 (endpoint unregistered, body `{"error":"agent_not_running"}`) and the Runtime-side 503 (body `{"error":"session_not_ready"}`). The two bodies differ in the `error` field, so the frontend can decide whether to "wait for a `status` push" or "retry".

### D4b: Gateway proxy status-code pass-through (new in v2)

Fixing the §2.6 collapse defect in `send_runtime_json` (`proxy.rs:2658-2710`):

1. Unregistered endpoint: `404 "Agent is not running"` → **`503 {"error":"agent_not_running"}` + `Retry-After: 2`** (aligned with the existing 503 contract of `proxy_to_runtime`; `with503Retry` can be reused as-is);
2. Runtime returns non-2xx: **pass the status code and body through**, no uniform mapping to `ApiError::not_found`;
3. Re-check whether `/api/agents/{id}/conversations/latest` (`chat.rs`) still has consumers — the Desktop main path uses `/latest-session` (`proxy_latest_session` → `proxy_to_runtime`, which passes through by itself); if no consumer is confirmed, **delete that route**, removing a side path that needs semantic maintenance.

### D5: Desktop consumption — zero new subscriptions

The `status` snapshot is already forwarded by the Desktop's `agent-event` channel (the `AgentStatusSnapshot` in `workspaceFsEvents.ts`); `state` / `detail` arrive for free with the `AgentStatus` extension, **no new MQTT subscription needed** (the v1 draft's "add an `agents/+/lifecycle` subscription" is cancelled with the topic merge). The ADR-058 §3.4 gap (`ready` observable only via 30s polling) is thereby closed.

Also delete the 6 `meta.ready` consumption sites (`ChatPanel.tsx:1152,1231`, `RightPanel.tsx:381,388`, and `waitForAgentReady` at `agentStore.ts:740`), all switching to `state`; `UNSPECIFIED` and `OFFLINE` gate identically (deny), but the UI displays them distinctly (§10 Q2).

### D6: No compatibility code

The user has decided "no old-version compatibility needed". Therefore:

- The `ready` bool is deleted rather than marked `reserved`; the `ready` topic constant is deleted;
- The three fields `AgentListResponse.ready` / `AgentDetailResponse.ready` / `RunningAgentInfo.ready` are deleted outright;
- `AgentStatus`'s `state` / `detail` are **new field numbers** (6/7); old decoders silently skip them — but the `ready` topic is already deleted, so an old Gateway receives no readiness signal at all, manifesting as "the agent is starting forever" rather than silent bad data. Runtime and Gateway must be deployed at the same version; this is an acceptable failure mode.

### D7: Remove the Runtime's up-front initial session creation (new in v2)

Remove the else branch at `session_init.rs:966-995` (when there is no recoverable session, `create_session()` + `set_latest_session` + `set_session_visibility(Private)`). Reasons in §2.7:

1. **single writer** (invariant 7): a session can only be created by the `POST /sessions` that carries the account identity, "owned and private from birth" (ADR-076 §decision 4, verbatim); no more ownerless garbage;
2. **zero sessions is a legal state**: `/sessions/latest` honestly returns `404 no_session`, and the Desktop creates one after `SESSIONS_READY` — this is exactly the trigger condition of decision 10, and the two paths close here;
3. that branch's existing rationale ("the frontend ChatPanel stays blank") depends on the old frontend contract this ADR deprecates;
4. without removing it, the core regression assertion in §8.2 ("delete all sessions → restart → 0 untitled") **necessarily fails**, and the test cannot land.

The if branch (resume + token merge) for when a session **is** recoverable is **left untouched** — that restores an existing session, it does not create a new one.


## 5. Rejected Alternatives

| Option | Reason for rejection |
|---|---|
| **A. Keep `ready` + add `sessions_ready`** | Explicitly rejected by the user. Per §1.2 decision 4: two bools have 4 combinations, of which `ready=false, sessions_ready=true` is **logically illegal**. An expressible illegal state = the state machine is not fully defined. |
| **B. Phase naming `loading_a/loading_b/.../ready`** | Proposed by the user; this ADR adopts the direction but rejects the form. See D2. |
| **C. Frontend-only fix (retry longer + don't clear the list)** | Treats the symptom. The protocol layer still cannot express "not ready", so the frontend is forever guessing; and it does not fix the §2.5 comment/implementation split. |
| **D. Only make the Runtime return a more accurate status code, no enum** | Solves D4 but not the startup period (inside the Gateway 503 window HTTP is unreachable, and the frontend gets nothing). Both are needed. |
| **E. The Gateway caches `/sessions` results to close the window** | Trades consistency for timing. The Gateway would violate the ADR-009 §5 boundary (agent-private data is read/written only via Runtime HTTP). |
| **F. Keep `ready` as a derived field (`state >= HTTP_READY`)** | Explicitly rejected by the user ("deprecate ready"). A derived field makes "two concepts coexist" — that is option A's problem under a different name. |
| **G. An independent `acowork/agents/{instance_id}/lifecycle` topic (v1 draft's lean)** | Rejected in v2. The LWT is one per connection and hangs on `status`: with an independent topic, after a crash the retained lifecycle stays **permanently stale at `SESSIONS_READY`**, violating invariant 4; the remedies (a Desktop cross-topic priority rule / the Gateway relaying lifecycle=offline) are all implicit contracts or a third writer — the very class of defect that broke `ready`. And the Gateway stop path (`agents.rs:1947`) already writes `status` twice, so merging gets `OFFLINE` for free. See D1. |

---

## 6. Consequences

### 6.1 Positive

- Within the startup window the frontend has **authoritative state** to read, no more guessing; after a crash retained state is backstopped by the LWT and does not go stale.
- "Not ready" vs "none" is distinguishable at the protocol level, and via D4b runs across the Gateway proxy layer → the three stacked defects in §2.4 and the collapse defect in §2.6 disappear at the source.
- Session creation converges to a single writer (D7); ownerless garbage sessions are no longer produced.
- The `FAILED` state makes startup failure visible to the user (today `degraded_reasons` only goes into `/health`, which the UI does not read).
- Eliminates the §2.5 comment/implementation split.
- Zero new topics, zero new subscriptions, zero new encoders — a smaller blast radius than the v1 draft.

### 6.2 Negative / cost

- A contract change across 3 layers; Runtime/Gateway/Desktop must be the same version.
- The 6 Desktop `ready` consumer sites + 2 session decision sites must change.
- The publication points of `AgentStatus` expand from "connect/disconnect" to "every capability boundary" (~4 sites); the monotonicity guard and re-stamp must be correct (D3b).
- All callers of `send_runtime_json` must re-check their dependence on 404 (after switching to pass-through, some callers will receive different status codes).

### 6.3 Boundaries / exceptions

- `local` mode: the state machine is fully consistent; the Desktop gates on `state` the same way.
- **standalone mode** (the `mqtt_client.is_none()` branch at `cli.rs:290`): no MQTT connection, no Gateway, no Desktop consumer — **the state machine does not apply at all** that branch publishes no state, and there is no "stuck at `HTTP_READY`" problem (the v1 draft §6.3 concern about `cli.rs:340` was a misreading, now closed, see §10 Q3). Implementation only needs a code comment noting "lifecycle constrains Gateway mode only".

### 6.4 Rollback

Deleting the `ready` topic makes old and new versions mutually unintelligible; a rollback requires a **whole rollback** (Runtime + Gateway + Desktop). Since the Desktop is a local Tauri app shipped with the installer, the rollback granularity is effectively "roll back the whole installer", which is acceptable.

### 6.5 Known technical debt

- `state` is a single enum; if "partial capability degradation" ever appears (e.g. sessions available but workspaces not), a redesign will be needed (it can then evolve into a capability bitmask; just add a field to `AgentStatus`, the topic and subscription paths are unchanged). The current phase's (`HTTP_READY` → `SESSIONS_READY`) capability split is sufficient.
- The `detail` of `FAILED` is overwritten by the LWT's `OFFLINE` if the process then crashes. Acceptable: a crash ≠ a startup failure; the log leaves a trace. When the Gateway receives `FAILED`, it drops an info log as a second trace.

---

## 7. Change List

### 7.1 `core/acowork-core`
- `mqtt_proto`: add the `AgentLifecycleState` enum; add `state = 6` / `detail = 7` fields to `AgentStatus`.
- Delete the `agents/+/ready` topic-related constants.
- Extend `encode_agent_status_payload` with `state` / `detail` parameters (the LWT payload fills `online=false, state=OFFLINE`).

### 7.2 `core/acowork-runtime`
- `mqtt/client.rs`: `publish_ready` → `publish_lifecycle(state, detail)` (with the built-in monotonicity guard); `ready_ever: AtomicBool` → `current_lifecycle: AtomicU8`; `run_bootstrap` publishes `STARTING` on first connection, and Step 7 re-stamps the current value on reconnect (D3b).
- `startup/agent_init.rs:582`: `publish_ready(true)` → `publish_lifecycle(HTTP_READY)`.
- `startup/session_init.rs`: **add** `publish_lifecycle(SESSIONS_READY)` after both slots are filled; **delete** the up-front initial session creation in the else branch at `:966-995` (D7, the resume branch stays).
- `http/server.rs`: `get_latest_session` / `list_sessions` etc. return `503 session_not_ready` per slot state; the 404 body is unified to `{"error":"no_session"}` (the D4 table).

### 7.3 `core/acowork-gateway`
- `mqtt/dispatch.rs`: delete the `acowork/agents/+/ready` branch; the existing `acowork/agents/+/status` branch is extended to parse `state` / `detail`.
- `gateway/state.rs`: `RunningAgentInfo.ready: bool` → `lifecycle: AgentLifecycleState`; delete `set_agent_ready`.
- `http/agents.rs`: delete `AgentListResponse.ready` / `AgentDetailResponse.ready`, add the `lifecycle` field; the stop path (`:1947-1961`) offline envelope gains `state=OFFLINE`; on receiving `FAILED`, drop an info log.
- `http/proxy.rs`: `send_runtime_json` changes an unregistered endpoint to `503 agent_not_running`; non-2xx passes the status code and body through (D4b).
- `http/chat.rs`: re-check `/conversations/latest` consumers; delete the route if none.

### 7.4 `apps/acowork-desktop`
- **No new subscription** (`state` arrives with the `AgentStatusSnapshot` of the `agent-event` channel); the `AgentStatusSnapshot` type gains `state` / `detail`.
- `stores/agentStore.ts`: `meta.ready` → `meta.lifecycle`; `waitForAgentReady` now waits for `SESSIONS_READY`; `fetchLatestSession` becomes tri-state (`ready` / `none` / `unavailable`); `fetchSessions` no longer clears the list on failure.
- `lib/agent-start.ts` + `agentStore.selectAgent`: the `createSession` trigger is tightened to "`SESSIONS_READY` + genuinely zero sessions".
- `UNSPECIFIED` gates as `OFFLINE`; the UI displays "state unknown (version mismatch?)" distinctly.

### 7.5 `dev/ci.sh`
- `run_gateway_fs_redline` needs no change (this ADR adds no Gateway fs access).
- The lint proposed by the v1 draft ("`publish_lifecycle` call sites must not be 0") is **cancelled** — it guards the wrong thing (D3b shows that in reconnect scenarios the call sites being "too many" is what is dangerous). Preventing a missed `SESSIONS_READY` publication is covered by the §8.1 monotonicity unit test + the §8.2 e2e.


## 8. Test Strategy

### 8.1 Unit tests
- **Runtime**: `lifecycle` monotonicity — after publishing `SESSIONS_READY`, it must not regress to `HTTP_READY`; **no regression on reconnect** — simulate the `run_bootstrap` Step 7 reconnect and assert the re-stamp is the current value, not `STARTING` (D3b).
- **Runtime**: with the slot unfilled, `GET /sessions/latest` returns `503 session_not_ready` (**not** 404); when ready with no session, returns `404 no_session`; when not readable, the body is likewise `no_session`.
- **Runtime**: the startup path (the zero-session scenario) produces **no** `create_session` call (D7).
- **Gateway**: parsing of each `AgentStatus` `state` value; an unknown value → `UNSPECIFIED` → gated as `OFFLINE`.
- **Gateway**: `send_runtime_json` pass-through — when the Runtime returns 503 the caller receives 503 (not 404); an unregistered endpoint returns `503 agent_not_running` (D4b).

### 8.2 Integration tests (e2e)
- **Core regression** (why this ADR exists): start the agent → no `POST /sessions` request may occur **before** `SESSIONS_READY`. Assert in the Gateway log that the `POST /api/agents/{id}/sessions` timestamp is later than the `state=SESSIONS_READY` publication time.
- Delete all sessions → restart → assert that the number of sessions with `message_count=0 AND title IS NULL` is 0 (**depends on D7**: otherwise the Runtime's up-front ownerless session makes the assertion self-defeat) → assert the Desktop creates exactly one owned session after `SESSIONS_READY`.
- The `FAILED` state: inject a startup failure → assert the Desktop receives `FAILED` + `detail`.
- The crash state: kill the Runtime process → assert the retained `AgentStatus` after the LWT is `online=false, state=OFFLINE` (not stale).

### 8.3 Protocol compatibility
- N/A (D6 settled no cross-version deployment). But the release notes must state that Runtime/Gateway/Desktop must be the same version.

## 9. Implementation Milestones

| Phase | Content | Verification |
|---|---|---|
| M1 | proto (the `AgentStatus` extension) + Runtime `publish_lifecycle` (atomic state + monotonic guard + reconnect re-stamp) + removing the up-front session creation (D7) | monotonicity/reconnect unit tests, the zero-session startup unit test |
| M2 | Runtime session interfaces `503 session_not_ready` + unified 404 body | unit tests |
| M3 | Gateway: the `status` branch parses `state`, delete the `ready` branch, `send_runtime_json` pass-through (D4b), the stop path gains `state=OFFLINE` | unit tests |
| M4 | Desktop: the `AgentStatusSnapshot` extension + state-driven decisions | e2e: no `POST /sessions` during startup; state not stale after a crash |

## 10. Open Questions (all decided in v2)

1. **Should `lifecycle` be independent of the `status` topic? — Decision: not independent, merged into `AgentStatus`** (D1, §5 option G). The LWT is one per connection and hangs on `status`; an independent topic's retained state is permanently stale after a crash; the Gateway stop path already writes `status` twice; the Desktop `agent-event` channel already forwards the `status` snapshot, so merging adds zero subscriptions. The v1 draft's "lean toward an independent topic" conclusion is overturned.
2. **Is `UNSPECIFIED` equivalent to `OFFLINE`? — Decision: equivalent for gating, distinct for display**. For capability gating, `UNSPECIFIED` and `OFFLINE` are treated the same (an unknown state must not be treated as available — the conservative side of invariant 1); the UI renders them distinctly — `OFFLINE` shows "not started", `UNSPECIFIED` shows "state unknown (version mismatch?)". Under D6's same-version deployment the only realistic cause of `UNSPECIFIED` is a deployment error; a clear hint saves one debugging trip over "starting forever". **No** version negotiation/handshake protocol (YAGNI).
3. **Is the `cli.rs:340` branch that bypasses Phase B still reachable? — Decision: closed, no terminal state needed**. Code fact: that branch is the else of `cli.rs:290`'s `if agent_ctx.mqtt_client.is_some()`, **only reachable in standalone mode** (verbatim comment: "this branch only runs when there is no MQTT client"). Standalone mode has no MQTT connection, no Gateway, no Desktop consumer, so the state machine does not apply at all, and there is no "stuck at `HTTP_READY`" problem (it publishes no state whatsoever). The v1 draft §6.3 concern was a misreading; implementation just needs a code comment noting "lifecycle constrains Gateway mode only".

## 11. Implementation Record (2026-10-01, after review)

After implementation + code review, three execution-level decisions landed. They do not change this ADR's conclusions; they only record "how it was done":

1. **D4's "scan unfinished → 503" is implemented via a process-level lifecycle-stamp read-back**. `get_latest_session`'s empty `latest_session` cache answers a definitive 404 only after the process has stamped `SESSIONS_READY` (that stamp is published after the background scan seeding completes, see D3); otherwise `503 session_not_ready`. The decision logic is converged into a pure function `empty_cache_is_definitive(cur_lifecycle, has_mqtt_client)` (`http/server.rs`); a `FAILED` stamp likewise answers 503 (the session subsystem was never ready, so 404 would be dishonest); standalone (no MQTT client) = no lifecycle authority, keeping the immediate 404 (§10 Q3). The Runtime thereby gains protocol-level self-protection, and §7.4's "tightened `createSession` trigger" is guaranteed by the protocol.
2. **No second `createSession` gate based on the store `meta.lifecycle` on the frontend**. Review had proposed a store-side gate in `agentStore.selectAgent`; rejected: MQTT pushes are delayed, the store field may lag the Runtime's real state, and rejecting a protocol-confirmed 404 on a stale field would pin the user in "Loading session…". The single source of truth is in the protocol layer (item 1).
3. **`FAILED` detail is latched on the frontend** (`agentStore.lastStartupFailure`). The FAILED→LWT OFFLINE window is millisecond-scale; after the process exits the Gateway's `running_agents` entry is removed and REST cannot recover the detail; when `updateAgentLiveness` receives `failed` it event-drives the latch, clears it on `sessions_ready` or a new `startAgent`, and `waitForAgentReady`'s error path reads the latch first. §1.2's "FAILED visible to the user" is thereby closed (the `state.rs` `lifecycle_detail` REST field is only valid during the process's lifetime; the comment states this honestly).

**Test status**: §8.1 unit tests all landed (added `empty_cache_gate_follows_sessions_ready_stamp`, `test_status_unknown_state_degrades_to_unspecified`, `agentStore.startupFailure.test.ts`); §8.2 e2e is a follow-up.
