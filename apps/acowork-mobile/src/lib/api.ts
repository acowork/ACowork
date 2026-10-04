/**
 * The real Gateway transport. Split from `chatStore` so the store stays a
 * pure state machine (and therefore testable without HTTP).
 *
 * BOUNDARY NOTE (ADR-009 §5): the mobile app NEVER reads the agent's
 * install_path, workspace, or private data off the local filesystem. Every
 * byte of agent-private content arrives through these HTTP calls. That is
 * not a stylistic preference — since ADR-055 `install_path` is node-local,
 * a filesystem shortcut would work on one machine and 5xx the moment the
 * Gateway and Node live on different hosts.
 *
 * AUTH (v1.1 §7): the Gateway is the only server; every call except
 * `/api/status` and `/api/auth/*` carries the Bearer access token. A 401
 * rotates the token pair exactly once (single-flight, so parallel 401s
 * cannot double-rotate and trip the Gateway's reuse detection) and replays
 * the request; a failed rotation reports `sessionExpired` and the app
 * returns to the login screen. Same ladder as Desktop's `authFetch`.
 */

import type { ChatTransport } from '../stores/chatStore'
import type {
  AccountMe,
  AgentRow,
  ChatMessage,
  ConversationEntry,
  DirectoryEntry,
  GatewayStatus,
  LiveStatus,
  MessagesPage,
  SessionDetail,
  SessionInfo,
  SessionSnapshot,
  SessionRow,
  SessionsPage,
  TokenPair,
} from './types'

/** Thrown for any non-2xx Gateway response; `status` drives the auth ladder. */
export class GatewayError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`Gateway ${status}: ${body.slice(0, 200)}`)
    this.name = 'GatewayError'
  }
}

/** Everything the transport needs from the auth layer, injected at boot so
 *  this module never imports a store (keeps the dependency one-directional). */
export interface AuthBridge {
  baseUrl(): string
  accessToken(): string | null
  /** Rotate the pair. Resolve with the new access token, or null on failure. */
  refresh(): Promise<string | null>
  /** Rotation failed: the session is gone. */
  sessionExpired(): void
}

let auth: AuthBridge | null = null

export function setAuthBridge(a: AuthBridge): void {
  auth = a
}

/** Public for the onboarding screens (they probe before any bridge exists). */
export async function probeStatus(baseUrl: string): Promise<GatewayStatus> {
  const res = await fetch(`${trim(baseUrl)}/api/status`, { headers: { Accept: 'application/json' } })
  if (!res.ok) throw new GatewayError(res.status, await res.text().catch(() => ''))
  return (await res.json()) as GatewayStatus
}

export async function loginRequest(baseUrl: string, username: string, password: string): Promise<TokenPair> {
  const res = await fetch(`${trim(baseUrl)}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
  if (!res.ok) throw new GatewayError(res.status, await res.text().catch(() => ''))
  return (await res.json()) as TokenPair
}

export async function refreshRequest(baseUrl: string, refreshToken: string): Promise<TokenPair> {
  const res = await fetch(`${trim(baseUrl)}/api/auth/refresh`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ refresh_token: refreshToken }),
  })
  if (!res.ok) throw new GatewayError(res.status, await res.text().catch(() => ''))
  return (await res.json()) as TokenPair
}

export async function logoutRequest(baseUrl: string, refreshToken: string): Promise<void> {
  await fetch(`${trim(baseUrl)}/api/auth/logout`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ refresh_token: refreshToken }),
  }).catch(() => undefined) // best-effort: a dead token is not an error here
}

function trim(u: string): string {
  return u.replace(/\/+$/, '')
}

/**
 * One authenticated request with the 401→refresh→replay ladder.
 * `path` must start with `/api`.
 */
async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const a = auth
  if (!a) throw new Error('auth bridge not installed')
  const send = async (token: string | null): Promise<Response> => {
    const headers: Record<string, string> = { Accept: 'application/json' }
    if (body !== undefined) headers['Content-Type'] = 'application/json'
    if (token) headers.Authorization = `Bearer ${token}`
    return fetch(`${trim(a.baseUrl())}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  }

  let res = await send(a.accessToken())
  if (res.status === 401) {
    // Single-flight lives in the bridge (authStore); awaiting it here also
    // dedupes PARALLEL 401s into one rotation. Re-send through the bridge
    // rather than a captured token: the rotation that answered our 401 may
    // itself have been triggered by an earlier request.
    const rotated = await a.refresh()
    if (!rotated) {
      a.sessionExpired()
      throw new GatewayError(401, 'session expired')
    }
    res = await send(a.accessToken())
  }
  if (!res.ok) throw new GatewayError(res.status, await res.text().catch(() => ''))
  if (res.status === 204) return undefined as T
  // Some accepted responses (e.g. 202 on send) carry no body.
  const text = await res.text()
  if (!text) return undefined as T
  return JSON.parse(text) as T
}

/* ------------------------------------------------------------------ */
/* Directory                                                          */
/* ------------------------------------------------------------------ */

export async function fetchAgents(): Promise<AgentRow[]> {
  return req<AgentRow[]>('GET', '/api/agents')
}

export async function fetchDirectory(): Promise<DirectoryEntry[]> {
  const r = await req<{ users: DirectoryEntry[] }>('GET', '/api/users/directory')
  return r.users ?? []
}

export async function fetchMe(): Promise<AccountMe> {
  return req<AccountMe>('GET', '/api/auth/me')
}

/* ------------------------------------------------------------------ */
/* Sessions & messages                                                */
/* ------------------------------------------------------------------ */

/** ISO timestamps → epoch ms, 0 when unparseable (never NaN into a store). */
function isoToMs(ts: string | null | undefined): number {
  if (!ts) return 0
  const n = Date.parse(ts)
  return Number.isFinite(n) ? n : 0
}

export function mapSessionRow(r: SessionRow): SessionInfo {
  return {
    session_id: r.session_id,
    title: r.title ?? '新会话',
    can_write: r.can_write,
    visibility: r.visibility ?? 'public',
    updated_at: isoToMs(r.last_active_at),
  }
}

export async function fetchSessions(agentId: string, page: number): Promise<{ items: SessionInfo[]; hasMore: boolean }> {
  const r = await req<SessionsPage>('GET', `/api/agents/${agentId}/sessions?page=${page}&size=20`)
  return { items: (r.sessions ?? []).map(mapSessionRow), hasMore: (r.page ?? page) < (r.total_pages ?? page) }
}

/**
 * Conversation JSONL → chat bubbles. v1 renders user/assistant text;
 * `tool_call` entries become collapsed tool rows, `tool_result` folds into
 * the preceding tool call, `thought`/`system`/`compaction` are not surfaced
 * (same reader contract as Desktop; `metadata.internal` is skipped).
 */
export function mapConversationEntries(entries: ConversationEntry[]): ChatMessage[] {
  const out: ChatMessage[] = []
  for (const e of entries) {
    if (e.kind === 'compaction') continue
    const internal = (e.metadata as { internal?: boolean } | undefined)?.internal === true
    if (internal) continue
    switch (e.role) {
      case 'user':
      case 'assistant':
        out.push({
          id: e.id,
          role: e.role,
          content: e.content,
          created_at: isoToMs(e.ts),
          kind: 'text',
        })
        break
      case 'tool_call': {
        const name = (e.metadata as { tool_name?: string } | undefined)?.tool_name ?? 'tool'
        out.push({
          id: e.id,
          role: 'assistant',
          content: e.content,
          created_at: isoToMs(e.ts),
          kind: 'text',
          payload: { type: 'tool_call', name },
        })
        break
      }
      default:
        // thought / tool_result / system: not rendered in v1
        break
    }
  }
  return out
}

export async function fetchMessages(agentId: string, sessionId: string): Promise<ChatMessage[]> {
  // tail=true, limit=100: the initial window is the most recent entries —
  // older pages arrive by explicit "load earlier" (v1.1 §4.3).
  const r = await req<MessagesPage>('GET', `/api/agents/${agentId}/sessions/${sessionId}/messages?tail=true&limit=100`)
  return mapConversationEntries(r.messages ?? [])
}

export async function fetchSessionDetail(agentId: string, sessionId: string): Promise<SessionDetail> {
  return req<SessionDetail>('GET', `/api/agents/${agentId}/sessions/${sessionId}`)
}

/** `live_state.status` — the only field the poll loop consumes (§8). */
export function extractStatus(d: SessionDetail): LiveStatus | null {
  const raw = d.live_state?.status
  if (!raw || typeof raw !== 'object' || typeof (raw as LiveStatus).status !== 'string') return null
  return raw as LiveStatus
}

/**
 * The poll primitive: status + the authoritative `meta.message_count`.
 * The count makes freshness detection stateless — a turn that starts and
 * finishes between two ticks still changes the count, so no transition can
 * be missed (v1.1 §8; fixes the fast-reply reload race).
 */
export function extractSnapshot(d: SessionDetail): SessionSnapshot {
  return { status: extractStatus(d), messageCount: d.meta?.message_count ?? null }
}

/**
 * Unverified JWT payload claims — CLIENT-SIDE USE ONLY (MQTT CONNECT
 * username + expiry check). The broker verifies the token itself; nothing
 * security-relevant is decided from these claims.
 */
export function tokenClaims(jwt: string): { sub?: string; exp?: number } {
  try {
    const part = jwt.split('.')[1]
    if (!part) return {}
    const json = JSON.parse(atob(part.replace(/-/g, '+').replace(/_/g, '/'))) as Record<string, unknown>
    return {
      sub: typeof json.sub === 'string' ? json.sub : undefined,
      exp: typeof json.exp === 'number' ? json.exp : undefined,
    }
  } catch {
    return {}
  }
}

/** Sends a user message; resolves with the client-generated message id so
 *  the store can key the optimistic bubble and its retry. */
export async function sendMessage(agentId: string, sessionId: string, content: string): Promise<string> {
  const messageId = crypto.randomUUID()
  await req('POST', `/api/agents/${agentId}/sessions/${sessionId}/messages`, {
    content,
    message_id: messageId,
  })
  return messageId
}

export async function sendApproval(agentId: string, sessionId: string, requestId: string, approved: boolean): Promise<void> {
  await req('POST', `/api/agents/${agentId}/sessions/${sessionId}/approval`, {
    request_id: requestId,
    approved,
  })
}

/** Answer an `ask_user_question` prompt (Desktop parity: lib/session-control). */
export async function sendAnswer(agentId: string, sessionId: string, requestId: string, answer: string): Promise<void> {
  await req('POST', `/api/agents/${agentId}/sessions/${sessionId}/answer`, {
    request_id: requestId,
    answer,
  })
}

export const httpChatTransport: ChatTransport = {
  async openSession(agentId, sessionId) {
    await req('POST', `/api/agents/${agentId}/sessions/${sessionId}/open`)
  },

  async fetchMessages(agentId, sessionId) {
    return fetchMessages(agentId, sessionId)
  },

  async fetchSessions(agentId, page) {
    return fetchSessions(agentId, page)
  },

  async fetchSnapshot(agentId, sessionId) {
    try {
      return extractSnapshot(await fetchSessionDetail(agentId, sessionId))
    } catch (e) {
      // 404 = the session is unknown to this runtime (never opened, or the
      // runtime restarted without it). That is idle, not an outage — the
      // poll loop must not raise the offline banner on it.
      if (e instanceof GatewayError && e.status === 404) return { status: null, messageCount: null }
      throw e
    }
  },

  async send(agentId, sessionId, content) {
    return sendMessage(agentId, sessionId, content)
  },

  async approval(agentId, sessionId, requestId, approved) {
    return sendApproval(agentId, sessionId, requestId, approved)
  },

  async answer(agentId, sessionId, requestId, answer) {
    return sendAnswer(agentId, sessionId, requestId, answer)
  },

  async createSession(agentId, title) {
    // visibility: 'private' is the on-disk default per ADR-076
    // create_frontend_session — a new session is nobody's until shared.
    // The create response only carries {session_id, created_at, owner,
    // visibility}; `can_write` is NOT in it, so the store re-reads the
    // authoritative row from page 1 of the list instead of trusting a
    // client-side guess (invariant: backend `can_write` is the only gate).
    const created = await req<{ session_id: string }>('POST', `/api/agents/${agentId}/sessions`, {
      visibility: 'private',
    })
    const { items } = await fetchSessions(agentId, 1)
    const row = items.find((x) => x.session_id === created.session_id)
    if (!row) throw new GatewayError(500, 'created session missing from list')
    // The title is set after creation via the config plane; v1 keeps the
    // runtime default and lets the first user message rename it (Desktop
    // parity), so `title` is only used for the optimistic label.
    void title
    return row
  },

  async deleteSession(agentId, sessionId) {
    await req('DELETE', `/api/agents/${agentId}/sessions/${sessionId}`)
  },
}
