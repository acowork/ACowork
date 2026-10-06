/**
 * User-to-user 1:1 chat (ADR-076 §决策 8).
 *
 * Deliberately a SEPARATE store from `chatStore`, not a special case inside
 * it. An agent conversation has sessions, `can_write`, a live status and an
 * MQTT channel; a human thread has none of those — it is a flat message log
 * with a server-side read cursor. Forcing both through one state machine
 * would mean a `null` sprinkled through every agent-only branch.
 *
 * The chat id is derived (`userChatId(me, peer)`), never looked up: a thread
 * exists as soon as its first message is sent, so there is no "create chat"
 * step and no way to open a thread that the server has not seen.
 */

import { create } from 'zustand'
import { fetchUserChats, fetchUserMessages, markUserChatRead, sendUserMessage, userChatId } from '../lib/api'
import { useAuthStore } from './authStore'
import type { UserChatMessage, UserChatSummary } from '../lib/types'

/**
 * Poll cadence for an open thread. Slower than the agent-session poll: a
 * human replies on a human timescale, and the MQTT bridge carries agent
 * sessions only — there is no push channel here to upgrade to.
 */
export const USER_CHAT_POLL_MS = 5000

/** The logged-in account id, or null until `/auth/me` has resolved. */
function meId(): string | null {
  return useAuthStore.getState().me?.user_id ?? null
}

interface UserChatStore {
  /** Threads this account has, most recent first — the inbox previews. */
  chats: UserChatSummary[]
  chatsLoaded: boolean
  /** Peer currently on screen; null = the inbox. */
  activePeerId: string | null
  activePeerName: string | null
  /** Oldest-first, exactly as the server returns them. */
  messages: UserChatMessage[]
  loading: boolean
  sending: boolean
  error: string | null

  refreshChats(): Promise<void>
  /** Open the thread with `peerId`; label falls back to the chat summary. */
  openChat(peerId: string, peerName?: string): Promise<void>
  closeChat(): void
  send(text: string): Promise<boolean>
  /** Silent refresh: the poll tick and the foreground return both call this. */
  refreshMessages(): Promise<void>
}

export const useUserChatStore = create<UserChatStore>((set, get) => ({
  chats: [],
  chatsLoaded: false,
  activePeerId: null,
  activePeerName: null,
  messages: [],
  loading: false,
  sending: false,
  error: null,

  refreshChats: async () => {
    const me = meId()
    if (!me) return
    try {
      set({ chats: await fetchUserChats(me), chatsLoaded: true })
    } catch {
      /* the inbox still renders the directory — a failed preview pull is not fatal */
    }
  },

  openChat: async (peerId, peerName) => {
    const me = meId()
    if (!me) return
    const chatId = userChatId(me, peerId)
    set({
      activePeerId: peerId,
      activePeerName:
        peerName ?? get().chats.find((c) => c.peer_user_id === peerId)?.peer_display_name ?? peerId,
      messages: [],
      loading: true,
      error: null,
    })
    try {
      const msgs = await fetchUserMessages(me, chatId)
      // Drop a stale page if the user switched threads while it was in flight.
      if (get().activePeerId !== peerId) return
      set({ messages: msgs })
      // Opening the thread IS reading it: clear the badge server-side, then
      // refetch the summaries so the inbox row stops claiming otherwise.
      await markUserChatRead(me, chatId).catch(() => undefined)
      void get().refreshChats()
    } catch (e) {
      if (get().activePeerId === peerId) set({ error: e instanceof Error ? e.message : '加载失败' })
    } finally {
      if (get().activePeerId === peerId) set({ loading: false })
    }
  },

  closeChat: () =>
    set({ activePeerId: null, activePeerName: null, messages: [], error: null, sending: false }),

  send: async (text) => {
    const me = meId()
    const peer = get().activePeerId
    const body = text.trim()
    if (!me || !peer || !body) return false
    set({ sending: true, error: null })
    try {
      const sent = await sendUserMessage(me, userChatId(me, peer), body)
      // Only append if the user has not switched threads mid-round-trip.
      if (get().activePeerId === peer) set((s) => ({ messages: [...s.messages, sent] }))
      // The first message also CREATES the thread, so refetch rather than
      // patching a row that may not exist yet.
      void get().refreshChats()
      return true
    } catch (e) {
      set({ error: e instanceof Error ? e.message : '发送失败' })
      return false
    } finally {
      set({ sending: false })
    }
  },

  refreshMessages: async () => {
    const me = meId()
    const peer = get().activePeerId
    if (!me || !peer) return
    try {
      const msgs = await fetchUserMessages(me, userChatId(me, peer))
      // Replace, never merge: the server page is the truth. A merge would
      // keep a message the server no longer returns.
      if (get().activePeerId === peer) set({ messages: msgs, error: null })
    } catch {
      /* transient network failure: keep showing what we have */
    }
  },
}))

/* One interval at a time, however many mount/unmount cycles the screen sees
 * (StrictMode double-mounts in dev). Same contract as `lib/realtime.ts`. */
let pollTimer: ReturnType<typeof setInterval> | null = null

export function startUserChatPoll(): void {
  if (pollTimer) return
  pollTimer = setInterval(() => void useUserChatStore.getState().refreshMessages(), USER_CHAT_POLL_MS)
}

export function stopUserChatPoll(): void {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}
