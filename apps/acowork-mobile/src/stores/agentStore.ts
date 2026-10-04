/**
 * The agent + contact directory, and the SINGLE OWNER of the session list.
 *
 * Why the session list lives here and not in `chatStore`: two stores both
 * holding `sessions` is how `can_write` silently stops being enforced —
 * the composer would consult one array while the permission hook consults
 * the other, and the read-only gate would fail open. One list, one owner.
 *
 * `chatStore` keeps only per-agent *view* state (which session is open, its
 * messages, loading flags) and reads the session list from here.
 */

import { create } from 'zustand'
import { fetchSessions } from '../lib/api'
import type { AgentSummary, SessionInfo, UserSummary } from '../lib/types'

/** Injected by `lib/api.ts` wiring at boot; keeps the store HTTP-free. */
export interface DirectorySource {
  loadAgents(): Promise<AgentSummary[]>
  loadUsers(): Promise<UserSummary[]>
}

let directory: DirectorySource | null = null

export function setDirectorySource(d: DirectorySource): void {
  directory = d
}

interface AgentStore {
  agents: Record<string, { info: AgentSummary; sessions: SessionInfo[] }>
  agentList: AgentSummary[]
  users: UserSummary[]
  /** True while the first directory load is in flight (list skeleton). */
  directoryLoading: boolean

  /** Pull agents + contacts; failure is reported, never swallowed silently. */
  refreshDirectory(): Promise<boolean>
  /** Page 1 of one agent's sessions — the IM preview needs them. */
  refreshSessions(agentId: string): Promise<void>

  setAgents: (list: AgentSummary[]) => void
  setUsers: (list: UserSummary[]) => void
  /** Prepend/merge a page of session history into the owning agent's slot. */
  mergeSessions: (agentId: string, items: SessionInfo[]) => void
  /** Remove a session and return the remaining list, for `chatStore`. */
  removeSession: (agentId: string, sessionId: string) => SessionInfo[]
  /** Read a single session row, the one place callers should get `can_write`. */
  getSession: (agentId: string, sessionId: string) => SessionInfo | undefined
}

export const useAgentStore = create<AgentStore>((set, get) => ({
  agents: {},
  agentList: [],
  users: [],
  directoryLoading: false,

  refreshDirectory: async () => {
    if (!directory) return false
    set({ directoryLoading: true })
    try {
      const [agents, users] = await Promise.all([directory.loadAgents(), directory.loadUsers()])
      get().setAgents(agents)
      get().setUsers(users)
      return true
    } catch {
      return false
    } finally {
      set({ directoryLoading: false })
    }
  },

  refreshSessions: async (agentId) => {
    if (!directory) return
    try {
      const { items } = await fetchSessions(agentId, 1)
      get().mergeSessions(agentId, items)
    } catch {
      /* a stale list beats an error screen for a preview fetch */
    }
  },

  setAgents: (list) =>
    set((s) => {
      // Preserve already-fetched session pages when the directory refreshes,
      // so a reconnect does not wipe the session switcher's list.
      const agents: AgentStore['agents'] = {}
      for (const a of list) {
        agents[a.id] = { info: a, sessions: s.agents[a.id]?.sessions ?? [] }
      }
      return { agentList: list, agents }
    }),

  setUsers: (list) => set({ users: list }),

  mergeSessions: (agentId, items) =>
    set((s) => {
      const slot = s.agents[agentId] ?? { info: { id: agentId, name: agentId }, sessions: [] }
      const known = new Set(slot.sessions.map((x) => x.session_id))
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...slot, sessions: [...slot.sessions, ...items.filter((i) => !known.has(i.session_id))] },
        },
      }
    }),

  removeSession: (agentId, sessionId) => {
    const remaining = (get().agents[agentId]?.sessions ?? []).filter((s) => s.session_id !== sessionId)
    set((s) => ({
      agents: {
        ...s.agents,
        [agentId]: { ...(s.agents[agentId] ?? { info: { id: agentId, name: agentId } }), sessions: remaining },
      },
    }))
    return remaining
  },

  getSession: (agentId, sessionId) => get().agents[agentId]?.sessions.find((s) => s.session_id === sessionId),
}))
