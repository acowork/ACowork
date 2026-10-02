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

/** The result of `chatStore.openSession` — a three-step atomic migration. */
export interface OpenSessionResult {
  ok: boolean
  /** Set when ok=false; surfaced verbatim to the user. */
  error?: string
}
