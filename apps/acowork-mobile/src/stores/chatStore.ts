/**
 * Per-agent VIEW state. The session list itself is owned by `agentStore` —
 * see the note there on why a second copy would break the `can_write` gate.
 *
 * The invariant this file exists to protect: a session switch is a
 * THREE-STEP ATOMIC MIGRATION, never a local page swap. Selecting a session
 * must (1) move the UI, (2) tell the Runtime `open_session` so it
 * re-points its active session on the wire, and (3) reload history over
 * HTTP. Skipping (2) leaves the Runtime streaming into the previous
 * session while the user reads the new one.
 *
 * Read-only sessions deliberately skip (2): `open_session` flips the
 * session's global Active/Closed lifecycle state, and a viewer has no
 * business opening someone else's session platform-wide. See ADR-086.
 */

import { create } from 'zustand'
import { useAgentStore } from './agentStore'
import type { ChatMessage, SessionInfo } from '../lib/types'

export interface AgentViewState {
  /** The session the user is looking at. May be a read-only view. */
  activeSessionId: string | null
  messages: ChatMessage[]
  /** True while history is loading, to keep the bubble list stable. */
  loading: boolean
  /** False once a load failed; the UI must not pretend a session is empty. */
  loaded: boolean
}

interface ChatStore {
  agentStates: Record<string, AgentViewState>
  /** The agent whose conversation detail is on screen; null = the IM list. */
  selectedAgentId: string | null
  /** Pagination cursor for the session history list (ADR-086 §分页). */
  historyPage: Record<string, number>
  historyHasMore: Record<string, boolean>

  selectAgent: (agentId: string | null) => void

  /**
   * The only supported way to change session. Returns ok=false on failure
   * so the caller can revert the UI instead of leaving it lying.
   */
  openSession: (agentId: string, sessionId: string) => Promise<boolean>

  /** Create a session and open it. Defaults to Private (ADR-076). */
  createSession: (agentId: string, title?: string) => Promise<string | null>

  deleteSession: (agentId: string, sessionId: string) => Promise<boolean>

  loadMoreSessions: (agentId: string) => Promise<void>
}

const emptyView = (): AgentViewState => ({
  activeSessionId: null,
  messages: [],
  loading: false,
  loaded: false,
})

/** Injected by `lib/api.ts`; kept out of the store so tests stay pure. */
export interface ChatTransport {
  openSession(agentId: string, sessionId: string): Promise<void>
  fetchMessages(agentId: string, sessionId: string): Promise<ChatMessage[]>
  fetchSessions(agentId: string, page: number): Promise<{ items: SessionInfo[]; hasMore: boolean }>
  createSession(agentId: string, title?: string): Promise<SessionInfo>
  deleteSession(agentId: string, sessionId: string): Promise<void>
}

let transport: ChatTransport | null = null

/** Wire the real HTTP transport once, at app boot. */
export function setChatTransport(t: ChatTransport): void {
  transport = t
}

export const useChatStore = create<ChatStore>((set, get) => ({
  agentStates: {},
  selectedAgentId: null,
  historyPage: {},
  historyHasMore: {},

  selectAgent: (agentId) => set({ selectedAgentId: agentId }),

  openSession: async (agentId, sessionId) => {
    if (!transport) return false
    const prev = get().agentStates[agentId] ?? emptyView()
    // Read the row from the ONE owner of the session list, so the read-only
    // decision below uses the same `can_write` the UI renders.
    const target = useAgentStore.getState().getSession(agentId, sessionId)
    const readOnly = target?.can_write === false

    // Step 1: move the UI. Optimistic — reverted below if the load fails.
    set((s) => ({
      agentStates: {
        ...s.agentStates,
        [agentId]: { ...prev, activeSessionId: sessionId, loading: true },
      },
    }))

    try {
      // Step 2: tell the Runtime, skipped for read-only sessions.
      if (!readOnly) await transport.openSession(agentId, sessionId)
      // Step 3: reload history.
      const messages = await transport.fetchMessages(agentId, sessionId)
      set((s) => ({
        agentStates: {
          ...s.agentStates,
          [agentId]: { ...prev, activeSessionId: sessionId, messages, loading: false, loaded: true },
        },
      }))
      return true
    } catch {
      // Revert to the previous session rather than showing a half-open state.
      set((s) => ({
        agentStates: { ...s.agentStates, [agentId]: { ...prev, loading: false } },
      }))
      return false
    }
  },

  createSession: async (agentId, title) => {
    if (!transport) return null
    const created = await transport.createSession(agentId, title)
    useAgentStore.getState().mergeSessions(agentId, [created])
    await get().openSession(agentId, created.session_id)
    return created.session_id
  },

  deleteSession: async (agentId, sessionId) => {
    if (!transport) return false
    await transport.deleteSession(agentId, sessionId)
    const remaining = useAgentStore.getState().removeSession(agentId, sessionId)
    set((s) => {
      const prev = s.agentStates[agentId] ?? emptyView()
      // Deleting the session the user is looking at must not leave a dangling
      // active id pointing at history that is gone.
      const wasActive = prev.activeSessionId === sessionId
      return {
        agentStates: {
          ...s.agentStates,
          [agentId]: {
            ...prev,
            ...(wasActive
              ? { activeSessionId: remaining[0]?.session_id ?? null, messages: [], loaded: false }
              : {}),
          },
        },
      }
    })
    return true
  },

  loadMoreSessions: async (agentId) => {
    if (!transport) return
    const hasMore = get().historyHasMore[agentId] ?? true
    if (!hasMore) return
    const page = (get().historyPage[agentId] ?? 0) + 1
    const { items, hasMore: more } = await transport.fetchSessions(agentId, page)
    useAgentStore.getState().mergeSessions(agentId, items)
    set((s) => ({ historyPage: { ...s.historyPage, [agentId]: page }, historyHasMore: { ...s.historyHasMore, [agentId]: more } }))
  },
}))
