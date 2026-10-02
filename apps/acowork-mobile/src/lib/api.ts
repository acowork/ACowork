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
 */

import type { ChatTransport } from '../stores/chatStore'
import type { ChatMessage, SessionInfo } from './types'

export interface GatewayConfig {
  baseUrl: string
  /** Bearer token for the Gateway; absent in `local` mode. */
  token?: string
}

let config: GatewayConfig = { baseUrl: '' }

export function setGatewayConfig(c: GatewayConfig): void {
  config = c
}

export function getGatewayConfig(): GatewayConfig {
  return config
}

class GatewayError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`Gateway ${status}: ${body.slice(0, 200)}`)
    this.name = 'GatewayError'
  }
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (config.token) headers.Authorization = `Bearer ${config.token}`

  const res = await fetch(`${config.baseUrl}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!res.ok) throw new GatewayError(res.status, await res.text().catch(() => ''))
  if (res.status === 204) return undefined as T
  return (await res.json()) as T
}

export const httpChatTransport: ChatTransport = {
  async openSession(agentId, sessionId) {
    await req('POST', `/agents/${agentId}/sessions/${sessionId}/open`)
  },

  async fetchMessages(agentId, sessionId) {
    return req<ChatMessage[]>('GET', `/agents/${agentId}/sessions/${sessionId}/messages`)
  },

  async fetchSessions(agentId, page) {
    // The page size lives here, not in the store: pagination arity is a
    // wire concern, and the store should not care how many rows come back.
    const r = await req<{ items: SessionInfo[]; has_more: boolean }>(
      'GET',
      `/agents/${agentId}/sessions?page=${page}&page_size=20`,
    )
    return { items: r.items ?? [], hasMore: !!r.has_more }
  },

  async createSession(agentId, title) {
    // visibility: 'private' is the on-disk default per ADR-076
    // create_frontend_session — a new session is nobody's until shared.
    return req<SessionInfo>('POST', `/agents/${agentId}/sessions`, {
      title: title ?? '新会话',
      visibility: 'private',
    })
  },

  async deleteSession(agentId, sessionId) {
    await req('DELETE', `/agents/${agentId}/sessions/${sessionId}`)
  },
}
