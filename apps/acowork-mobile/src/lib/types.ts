/**
 * Wire types shared by the stores. Mirrors the fields the Gateway already
 * returns — this is a subset of Desktop's, not a redesign. Anything the
 * mobile app does not render is omitted, and absence of a field is never
 * interpreted as a permission.
 */

/** ADR-076 session visibility. Presentation-only; never a write gate. */
export type SessionVisibility = 'public' | 'private'

/** One row of an agent's session history. */
export interface SessionInfo {
  session_id: string
  title: string
  /** ADR-076 §决策 4: the single authority for write access. */
  can_write?: boolean
  visibility?: SessionVisibility
  owner_id?: string | null
  updated_at?: number
  /** Ids of the last few messages, used to build the IM-style preview. */
  last_message?: string | null
}

/** An installed agent, as shown in the IM conversation list. */
export interface AgentSummary {
  id: string
  name: string
  icon?: string | null
  status?: 'idle' | 'busy' | 'error' | 'offline'
  /** Count of sessions this account has open/created. */
  session_count?: number
  unread?: number
}

/** A human contact. User-to-user chat is Gateway-side 1:1 with no session. */
export interface UserSummary {
  id: string
  display_name: string
  avatar?: string | null
  online?: boolean
  last_message?: string | null
  updated_at?: number
  unread?: number
}

/** A single chat bubble. */
export interface ChatMessage {
  id: string
  role: 'user' | 'assistant' | 'system'
  content: string
  created_at: number
  /** Only assistant messages can ask a question or request an approval. */
  kind?: 'text' | 'ask_question' | 'approval'
  payload?: unknown
}

/** The result of `chatStore.openSession` — kept for the screens' error UX. */
export type OpenSessionResult = { ok: true } | { ok: false; error: string }

/* ------------------------------------------------------------------ */
/* Gateway wire shapes (v1.1 §7/§8). Field names match the Rust       */
/* serializers exactly; optional-ness mirrors `skip_serializing_if`.  */
/* ------------------------------------------------------------------ */

/** `GET /api/status` — public, reachable before any login. */
export interface GatewayStatus {
  version: string
  agents_installed: number
  agents_running: number
  uptime_secs: number
  mqtt_port: number
  auth_mode: 'local' | 'multi_user'
  registration_open: boolean
  requires_setup: boolean
}

/** `POST /api/auth/login|refresh` — `TokenPair` (ADR-076 §决策 3). */
export interface TokenPair {
  access_token: string
  refresh_token: string
  token_type: string
  expires_in: number
}

/** `GET /api/auth/me` — the subset of `AccountView` the app renders. */
export interface AccountMe {
  user_id: string
  username: string
  display_name: string
  role: 'user' | 'admin'
}

/** `GET /api/users/directory` row. */
export interface DirectoryEntry {
  user_id: string
  username: string
  display_name: string
  avatar?: string | null
  builtin_avatar?: string | null
}

/** `GET /api/agents` row — only the fields the IM list needs. */
export interface AgentRow {
  instance_id: string
  agent_id: string
  name: string
  display_name?: string | null
  builtin_avatar?: string | null
  alive: boolean
  lifecycle: string
}

/** `GET .../sessions` row — `SessionSummary`. `can_write` is authoritative. */
export interface SessionRow {
  session_id: string
  title?: string | null
  created_at: string
  last_active_at: string
  message_count: number
  visibility?: SessionVisibility | null
  can_write: boolean
}

/** `GET .../sessions` envelope. */
export interface SessionsPage {
  sessions: SessionRow[]
  total_count: number
  total_pages: number
  page: number
  size: number
}

/** `GET .../messages` envelope — entries are raw `ConversationEntry` JSONL. */
export interface MessagesPage {
  session_id: string
  messages: ConversationEntry[]
  offset: number
  limit: number
  total: number
  count: number
}

/** One conversation JSONL entry. Roles beyond user/assistant exist and are */
/* filtered or folded by `mapConversationEntries`.                          */
export interface ConversationEntry {
  id: string
  ts: string
  role: 'user' | 'assistant' | 'thought' | 'tool_call' | 'tool_result' | 'system'
  content: string
  metadata?: unknown
  kind?: string | null
}

/**
 * `live_state.status` — the serde form of the Runtime's `SessionStatus`
 * (`#[serde(tag = "status", content = "detail", rename_all = "snake_case")]`).
 * v1.1 §8: the foreground poll loop reads ONLY this field.
 */
export type LiveStatus =
  | { status: 'idle' }
  | { status: 'llm_awaiting_first_chunk' }
  | { status: 'thinking' }
  | { status: 'llm_streaming'; detail?: { message_id?: string | null } }
  | { status: 'tool_executing' }
  | { status: 'waiting_approval'; detail: { request_id: string } }
  | { status: 'paused'; detail?: Record<string, unknown> }
  | { status: 'errored'; detail?: { reason?: string; message?: string; recoverable?: boolean } }

/** `GET .../sessions/{sid}` — the panel-4 detail (meta + live state). */
export interface SessionDetail {
  session_id: string
  meta: { session_id: string; created_at: string; last_active_at: string; message_count: number }
  live_state: { status: LiveStatus | null } | null
}

/**
 * What the foreground poll loop reads each tick (v1.1 §8): the live status
 * plus the authoritative `meta.message_count`. The count — not a observed
 * status transition — is the freshness signal, so a turn that starts and
 * finishes between two ticks still triggers a history reload.
 */
export interface SessionSnapshot {
  status: LiveStatus | null
  messageCount: number | null
}
