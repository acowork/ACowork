//! Session control plane over Gateway HTTP (ADR-076 §决策 4).
//!
//! These four operations used to be published as MQTT control commands
//! through the Tauri `mqtt_publish_control` command, which the Desktop
//! sent straight to the broker. That path carries no identity — the broker
//! cannot stamp one — so the Gateway could not authenticate the caller and
//! the Runtime could not record or check a session's owner. They now go
//! through the Gateway's authenticated HTTP API, which forwards the
//! resolved account as a header on the reverse proxy.
//!
//! (`create_session` could not return the new id over MQTT either; the
//! HTTP response carries it. The Desktop still waits for the
//! `session_created` event, so behaviour is unchanged there.)

import { getGatewayUrl } from "./config";

async function controlRequest(
  method: "POST" | "DELETE" | "PUT",
  path: string,
  body?: unknown,
): Promise<void> {
  const resp = await fetch(`${getGatewayUrl()}${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!resp.ok) {
    throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
  }
}

/** `POST /api/agents/{id}/sessions` — the new session is owned by the caller. */
export function createSession(
  agentId: string,
  body: { workspace_id?: string } = {},
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions`, body);
}

/** `POST /api/agents/{id}/sessions/{sid}/open` — ADR-038 activation, idempotent. */
export function openSession(agentId: string, sessionId: string): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/open`);
}

/**
 * `POST /api/agents/{id}/sessions/{sid}/close` — graceful close.
 *
 * Idempotent on the Runtime side (an already-closed session is a no-op),
 * so callers may fire it unconditionally.
 */
export function closeSession(agentId: string, sessionId: string): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/close`);
}

/** `DELETE /api/agents/{id}/sessions/{sid}` — removes the session and its files. */
export function deleteSession(agentId: string, sessionId: string): Promise<void> {
  return controlRequest("DELETE", `/api/agents/${agentId}/sessions/${sessionId}`);
}

/**
 * `PUT /api/agents/{id}/sessions/{sid}/workspace` — switch the session's
 * workspace.
 *
 * ADR-076 §决策 4: replaces the MQTT `workspace_switch` command. This is
 * *not* a `patchSessionConfig({ workspace_id })` call — the config path only
 * rewrites meta, leaving the session's tools pointed at the old directory
 * (see the Runtime-side `route_workspace_switch`).
 */
export function setSessionWorkspace(
  agentId: string,
  sessionId: string,
  workspaceId: string,
): Promise<void> {
  return controlRequest("PUT", `/api/agents/${agentId}/sessions/${sessionId}/workspace`, {
    workspace_id: workspaceId,
  });
}

/**
 * `PUT /api/agents/{id}/sessions/{sid}/config` — partial config update.
 *
 * ADR-076 §决策 4: replaces the MQTT `model_switch`, `reasoning_effort` and
 * `update_session_title` commands. Field names are the backend's
 * `SessionConfigDelta` names (snake_case `reasoning_effort`, not the
 * frontend's camelCase), and absent fields are left untouched.
 */
export function patchSessionConfig(
  agentId: string,
  sessionId: string,
  patch: {
    model?: string;
    provider?: string;
    /** Multi-account: which API key of the provider (empty = first). */
    account_id?: string;
    reasoning_effort?: string;
    title?: string;
    temperature?: number;
    context_window?: number;
  },
): Promise<void> {
  return controlRequest("PUT", `/api/agents/${agentId}/sessions/${sessionId}/config`, patch);
}

/**
 * `PUT /api/agents/{id}/sessions/{sid}/visibility` — share or unshare.
 *
 * `null` clears the field, restoring the public default (ADR-076 §决策 4:
 * absent means public, so an unshared-but-unset session is the same state
 * as a fresh one). Not yet wired to any UI — the toggle is reserved.
 */
export function setSessionVisibility(
  agentId: string,
  sessionId: string,
  visibility: "public" | "private" | null,
): Promise<void> {
  return controlRequest("PUT", `/api/agents/${agentId}/sessions/${sessionId}/visibility`, {
    visibility,
  });
}

// ─────────────────────────────────────────────────────────────────────
// Session *actions* (ADR-076 §决策 4, second wave)
//
// These eight calls used to be `invoke("mqtt_publish_control", …)`, which
// published straight to the broker. That path carries no identity, so
// anyone able to reach the broker could drive another account's session —
// including approving a risky tool call (`approved: true`), which is
// arbitrary command execution. They now go through the Gateway's
// authenticated HTTP API like the lifecycle calls above.
//
// The HTTP response only acknowledges routing (202): the action's outcome
// still arrives on the MQTT event plane, so callers keep treating these as
// fire-and-forget, exactly as before.
// ─────────────────────────────────────────────────────────────────────

/** `CompressType::SUMMARY` — the value the context-usage menu sends. */
export const COMPRESS_SUMMARY = 1;

/** `CompressType::CANCEL` (ADR-083) — abort the in-flight context compaction.
 *  Same endpoint as `SUMMARY`; the Runtime routes the `compress_type` to the
 *  running compaction's cancel handle. */
export const COMPRESS_CANCEL = 3;

/** `POST .../sessions/{sid}/messages` — send a user chat message. */
export function sendMessage(
  agentId: string,
  sessionId: string,
  body: {
    content: string;
    message_id: string;
    /** Slash-command *name* only; the Runtime resolves the instructions. */
    command?: string;
    /** Opaque JSON string (not an object) — the Runtime parses it. */
    params_json?: string;
  },
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/messages`, body);
}

/** `POST .../sessions/{sid}/stop` — interrupt the current generation. */
export function stopSession(
  agentId: string,
  sessionId: string,
  reason = "user_requested",
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/stop`, { reason });
}

/** `POST .../sessions/{sid}/continue` — resume after an iteration-limit pause. */
export function continueSession(
  agentId: string,
  sessionId: string,
  reason = "user_requested",
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/continue`, {
    reason,
  });
}

/**
 * `POST .../sessions/{sid}/approval` — the user's tool-risk decision.
 *
 * This is the call that used to be spoofable over MQTT.
 */
export function sendApproval(
  agentId: string,
  sessionId: string,
  body: {
    request_id: string;
    approved: boolean;
    allow_all_session?: boolean;
    reason?: string;
  },
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/approval`, body);
}

/** `POST .../sessions/{sid}/answer` — answer an `ask_user_question` prompt. */
export function sendAnswer(
  agentId: string,
  sessionId: string,
  body: { request_id: string; answer: string },
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/answer`, body);
}

/**
 * `POST .../sessions/{sid}/cancel-tool` — ADR-045: abort a single in-flight
 * tool. The surrounding iteration continues; an unknown `tool_call_id` is a
 * no-op (race against natural completion).
 */
export function cancelTool(
  agentId: string,
  sessionId: string,
  toolCallId: string,
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/cancel-tool`, {
    tool_call_id: toolCallId,
  });
}

/** `POST .../sessions/{sid}/compress` — user-initiated context compression. */
export function compressSession(
  agentId: string,
  sessionId: string,
  compressType: number = COMPRESS_SUMMARY,
): Promise<void> {
  return controlRequest("POST", `/api/agents/${agentId}/sessions/${sessionId}/compress`, {
    compress_type: compressType,
  });
}
