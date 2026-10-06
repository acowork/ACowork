# ADR-012: Per-Session Model/Provider Isolation

> **Chinese source of truth**: [ADR-012](../zh/ADR-012-per-session-model-isolation.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Proposed

## Date

2026-05-29

## Decision Makers

架构讨论 (architecture discussion)

## Blast radius

`chatStore.ts`, `ChatPanel.tsx`, `session_manager.rs`, `session_task.rs`, `cli.rs`, `conversation.rs`, `chat.rs` (Gateway)

---

## Context

The current model/provider is stored in the frontend `AgentState` (agent level) and
the backend `SessionManagerConfig.override_model` (agent level). `model_switch` is
pushed via `broadcast()` to every session, so switching the model in one session
affects all sessions under the same agent.

`workspace_id` already solves this: it lives in `SessionMetadata` (the JSONL first line),
persisted per session. Model/provider MUST follow the same per-session pattern.

## Decision

### Data model

**Frontend.** `SessionChatState` gains model/provider fields — the **only** storage for
model information:

```typescript
interface SessionChatState {
  // ... existing fields
  model: string | null;     // per-session model (single source)
  provider: string | null;  // per-session provider (single source)
}
```

**Backend.** `SessionMetadata` (JSONL first line) gains model/provider fields, same
pattern as `workspace_id`; `SessionState` (in memory) gains the same:

```rust
pub struct SessionMetadata {
    // ... existing fields
    pub model: Option<String>,      // per-session model
    pub provider: Option<String>,   // per-session provider
}

pub struct SessionState {
    // ... existing fields
    pub model: Option<String>,
    pub provider: Option<String>,
}
```

### Frontend changes

**1. `setCurrentModel` writes per session.** The write target moves to session level and
the WS message carries `session_id`:

```typescript
setCurrentModel: (model: string, provider: string, agentId: string) => {
  const sessionId = getAgentState(get(), agentId).activeSessionId;
  if (!sessionId) return;

  set((state) => updateSessionState(state, agentId, sessionId, { model, provider }));

  const ws = get().wsMap[agentId];
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: "model_switch",
      model, provider, agentId,
      session_id: sessionId,
    }));
  }
}
```

**2. UI reads from session level.** No fallback chain:

```typescript
// ChatPanel.tsx
const currentModel = sessionState?.model ?? null;
const currentProvider = sessionState?.provider ?? null;
```

**3. `activateSession` no longer syncs the model.** It only updates `activeSessionId`;
the UI recomputes automatically because `sessionState` is already selected by
`activeSessionId`, so after a switch it naturally reads the target session model.

**4. The `model_confirmed` event writes to session level.**

```typescript
case "model_confirmed": {
  const sessionId = getAgentState(state, confirmedAgentId).activeSessionId;
  if (sessionId) {
    return updateSessionState(state, confirmedAgentId, sessionId, {
      model: confirmedModel,
      provider: confirmedProvider ?? "",
    });
  }
}
```

**5. `setAvailableModels` simplified.** No fallback logic, only the available list.

**6. New session initialization.** A new session starts with `model: null` /
`provider: null`; the value is written when `model_confirmed` arrives or the user
picks a model.

### Backend changes

**1. New session model initialization.** No longer loaded from `agent_model.json`; a
new session starts from manifest `suggested_model`:

```rust
let initial_model = self.core.manifest.llm.suggested_model.clone();
session_state.set_initial_model(initial_model.clone());
```

A session restored from JSONL reads the model from metadata:

```rust
if let Some(ref conversation) = conversation {
    let meta = conversation.read_metadata();
    if let Some(model) = &meta.model {
        context_builder.set_override_model(model.clone());
    }
}
```

**2. Persist model/provider to JSONL.** Identical to the `workspace_id` pattern — written
to the `SessionMetadata` first line on creation, and updated on every `model_switch`:

```rust
fn update_session_model_provider(
    work_dir: &str, session_id: &str,
    model: &str, provider: Option<&str>,
) {
    let path = conversation_path(work_dir, session_id);
    if let Some(mut conversation) = ConversationSession::open(&path) {
        let mut meta = conversation.read_metadata();
        meta.model = Some(model.to_string());
        meta.provider = provider.map(|s| s.to_string());
        conversation.update_metadata(&meta);
    }
}
```

**3. `model_switch` routes to one session.** No `broadcast()`, no `save_agent_model()`:

```rust
if action == "model_switch" {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    if let Some(sid) = session_id {
        if let Some(handle) = session_manager.get_session(sid) {
            let _ = handle.send(SessionMessage::ModelSwitch {
                model: model.to_string(),
            });
            update_session_model_provider(work_dir, sid, model, provider);
        }
    } else {
        tracing::warn!("model_switch missing session_id, ignoring");
    }
    return LoopAction::Continue;
}
```

`SessionTask` then updates both `ContextBuilder` and `SessionState`:

```rust
Some(SessionMessage::ModelSwitch { model }) => {
    context_builder.set_override_model(model.clone());
    agent_loop.session.set_model(Some(model));
}
```

**4. `SessionTask::new()` drops the `override_model` parameter.** The initial model is
read from `SessionState` inside `SessionTask::run()`:

```rust
let initial_model = session_state.model()
    .unwrap_or_else(|| self.core.manifest.llm.suggested_model.clone());
context_builder = context_builder.with_override_model(initial_model);
```

**5. `QueryConfig` simplified.** It no longer reads a model; the model is managed
independently by each session:

```rust
let config_snapshot = ConfigSnapshot {
    tools: tool_definitions,
    available_models: session_manager.available_models(),
    // model field removed or left empty (managed per session by the frontend)
};
```

**6. Startup drops all `agent_model.json` code** — the `load_agent_model()` call, the
`saved_provider` fallback (provider selection now depends only on the manifest), the
`save_agent_model` + `update_model_override` cache block after `AgentHelloResult`, and
the `save_agent_model` before `process_gateway_recv` exits.

### Gateway change

`chat.rs` forwards `session_id` when present:

```rust
if let Some(sid) = client_msg.session_id {
    params["session_id"] = serde_json::json!(sid);
}
```

---

## Deletion list

| Location | Removed | Reason |
|----------|---------|--------|
| Frontend `AgentState` | `model`, `provider` | moved to `SessionChatState` |
| Frontend `ChatStore` | `currentModel`, `currentProvider`, `agentModels` | no global fields needed |
| Frontend `setAvailableModels` | fallback logic | no fallback needed |
| Frontend `setCurrentModel` | global field writes | writes session level only |
| Frontend `activateSession` | model/provider sync | derived by the UI selector |
| Backend `SessionManagerConfig` | `override_model` | replaced by `SessionState` |
| Backend `SessionManager` | `update_model_override()` | no longer needed |
| Backend `cli.rs` | `save_agent_model`, `load_agent_model`, `AgentModelEntry`, `AGENT_MODEL_FILE` | no agent-level persistence |
| Backend `SessionTask::new()` | `override_model` parameter | carried by `SessionState` |
| Backend `model_switch` | `broadcast()` call | single-session routing |
| Backend startup | `agent_model.json` reads/writes, AgentHello model cache | no longer needed |
| Backend `QueryConfig` | `load_agent_model` read | each session manages its own model |

### `loadAgentModel`

`loadAgentModel` (`chatStore.ts` L1047) calls `GET /api/agents/{agentId}/model` for the
agent-level model, invoked when ChatPanel starts the agent. After the move to per-session:

- the active session model already arrives via the `model_confirmed` event into `SessionChatState`
- `loadAgentModel` no longer needs the HTTP API
- **Decision**: delete the `loadAgentModel` HTTP call and read the active session model
  directly from `SessionChatState`
- ChatPanel L322–324 initialization becomes: `loadModels()` calls `setAvailableModels`
  directly, with no `await loadAgentModel` first

## Implementation steps

**Ordering principle**: frontend first (it sends `session_id`, which the old backend
ignores, so nothing breaks), then the backend.

**Phase 1 — frontend data model + UI**

1. `SessionChatState` gains `model: string | null`, `provider: string | null`; `DEFAULT_SESSION_STATE` initializes both to `null`
2. `ChatPanel.tsx` reads `sessionState?.model`, dropping the `agentState?.model` fallback
3. `setCurrentModel`: write via `updateSessionState` + carry `session_id` in the WS message (the old backend ignores it, no side effects)
4. `model_confirmed` handler: write to session level via `updateSessionState`
5. Delete `AgentState.model`, `AgentState.provider`
6. Delete `ChatStore.currentModel`, `currentProvider`, `agentModels`
7. Delete the prevModel revert logic in `setCurrentModel` (just return when there is no WS)
8. `setAvailableModels` sets only the list
9. `activateSession` drops the `currentModel`/`currentProvider` sync
10. Delete the HTTP fetch in `loadAgentModel`; ChatPanel initialization calls `loadModels()` directly

**Phase 2 — backend `SessionState` + persistence**

1. `SessionMetadata` gains `model: Option<String>`, `provider: Option<String>`
2. `SessionState` gains the fields plus setters/getters
3. New session: initial model = manifest `suggested_model`, written to the JSONL metadata
4. Restored session: read the model from metadata → set it on `context_builder`
5. Add the `update_session_model_provider()` persistence helper

**Phase 3 — backend `model_switch` rework + agent-level removal**

1. `model_switch`: route to the target session via `params["session_id"]`; delete `broadcast()`
2. Delete `save_agent_model()` / `load_agent_model()` / `AgentModelEntry` / `AGENT_MODEL_FILE`
3. Delete every `agent_model.json` read/write in the startup path
4. Delete the `override_model` parameter of `SessionTask::new()`
5. Delete `SessionManagerConfig.override_model` and `update_model_override()`
6. On `ModelSwitch`, update both `context_builder` and `SessionState`
7. `SessionTask::run()` reads the initial model from `SessionState` into `context_builder`
8. `QueryConfig` drops the `load_agent_model` read

**Phase 4 — Gateway forwarding + cleanup**

1. Gateway `chat.rs` forwards `session_id` into `params`
2. Verify a model switch affects only the target session
3. Verify a new session uses the manifest `suggested_model`
4. Verify the model is correctly restored after switching sessions
5. Verify `model_confirmed` correctly writes to session level
6. Sweep for leftover references to `agent_model` / `broadcast` / `override_model` / `currentModel` / `agentModels`

## Consequences

### Positive

- **Clean data model**: model/provider has exactly one storage location — no duplication, no derived state, no compatibility layer
- **True session isolation**: switching the model affects only the current session; switching sessions restores the right model
- **Less code**: 3 global frontend fields, 2 backend functions, 1 struct and 1 file constant removed
- **Consistent with the workspace pattern**: JSONL metadata becomes the single storage layer for all per-session data

### Negative

- Old JSONL files have no `model`/`provider` field: the `Option` is `None` and the session falls back to the manifest `suggested_model` (an acceptable old/new transition)
- `QueryConfig` no longer returns agent-level model information (the frontend no longer needs it)
