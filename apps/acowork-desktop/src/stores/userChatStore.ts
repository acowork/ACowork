/**
 * User↔user inbox state (ADR-076 §决策 8).
 *
 * Deliberately **not** folded into [chatStore](chatStore.ts): that store is
 * the agent-session chat (MQTT event streams, virtual list, tabs, tool
 * blocks, attachments) and shares no shape with a two-person inbox — an
 * agent conversation has a session id and a provider, a human one has a
 * `min__max` pair and an unread count. Merging them would branch ~3900
 * lines on "is this an agent or a person".
 *
 * Transport is plain HTTP + a poll while the view is mounted (there is no
 * MQTT topic for user chat — ADR-076 §决策 8 puts the whole feature on the
 * Gateway's HTTP surface).
 */

import { create } from "zustand";

import {
  listChats,
  listMessages,
  markChatRead,
  sendChatMessage,
  uploadChatAttachment,
} from "../lib/user-chat-api";
import type {
  ChatAttachment,
  UserChatMessage,
  UserChatSummary,
} from "../lib/types";
import { useAuthStore } from "./authStore";

interface UserChatState {
  /** Conversations of the signed-in user, most recently active first. */
  chats: UserChatSummary[];
  /** The peer whose thread is open, if any. */
  activePeerId: string | null;
  /**
   * Display label for `activePeerId`, when the caller already knows it (the
   * sidebar UserList does). Without it a brand-new conversation — one with
   * no `GET /chats` row yet — would title itself with a raw UUID.
   */
  activePeerLabel: string | null;
  messages: UserChatMessage[];
  /**
   * Uploaded but not yet sent, in upload order. Cleared only once a message
   * that carries them is accepted — an upload is not a delivery.
   */
  pendingAttachments: ChatAttachment[];
  /** File names currently uploading, for the composer's progress hint. */
  uploading: string[];
  loadingChats: boolean;
  loadingMessages: boolean;
  sending: boolean;
  error: string | null;

  refreshChats: () => Promise<void>;
  openChat: (peerId: string, peerLabel?: string) => Promise<void>;
  closeChat: () => void;
  reloadMessages: () => Promise<void>;
  /** Upload one file; returns false when the Gateway refused it. */
  attach: (file: File) => Promise<boolean>;
  removePendingAttachment: (id: string) => void;
  /**
   * Send the draft. Returns true when the Gateway accepted the message.
   * `attachmentIds` defaults to the pending set, which is then cleared.
   */
  send: (body: string, attachmentIds?: string[]) => Promise<boolean>;
  /**
   * Ref-counted poll loop: starts on the first caller (the nav badge, the
   * open inbox) and stops when the last one releases. Returns the release
   * function — call it from the effect cleanup.
   */
  startPolling: () => () => void;
  reset: () => void;
}

/** Canonical pair id — must match the Gateway's `min__max` (byte order). */
export function chatIdOf(a: string, b: string): string {
  return [a, b].sort().join("__");
}

/** Poll cadence. One interval exists at a time, however many callers. */
export const POLL_MS = 5000;

let pollTimer: ReturnType<typeof setInterval> | null = null;
let pollRefs = 0;

/** The signed-in user's id, or null when the account system is off. */
function selfId(): string | null {
  const { mode, account } = useAuthStore.getState();
  return mode === "multi_user" ? (account?.user_id ?? null) : null;
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

const EMPTY = {
  chats: [] as UserChatSummary[],
  activePeerId: null as string | null,
  activePeerLabel: null as string | null,
  messages: [] as UserChatMessage[],
  pendingAttachments: [] as ChatAttachment[],
  uploading: [] as string[],
  loadingChats: false,
  loadingMessages: false,
  sending: false,
  error: null as string | null,
};

export const useUserChatStore = create<UserChatState>((set, get) => ({
  ...EMPTY,

  refreshChats: async () => {
    const self = selfId();
    if (!self) return;
    set({ loadingChats: true });
    try {
      set({ chats: await listChats(self), error: null });
    } catch (err) {
      set({ error: describe(err) });
    } finally {
      set({ loadingChats: false });
    }
  },

  openChat: async (peerId, peerLabel) => {
    const self = selfId();
    if (!self) return;
    set({
      activePeerId: peerId,
      activePeerLabel: peerLabel ?? null,
      messages: [],
      pendingAttachments: [],
      error: null,
    });
    await get().reloadMessages();

    // Read receipt. A failure here must not blank the thread — the badge
    // just stays until the next poll.
    try {
      await markChatRead(self, chatIdOf(peerId, self));
      set((s) => ({
        chats: s.chats.map((c) =>
          c.peer_user_id === peerId ? { ...c, unread_count: 0 } : c,
        ),
      }));
    } catch {
      /* badge stays */
    }
  },

  closeChat: () =>
    set({
      activePeerId: null,
      activePeerLabel: null,
      messages: [],
      pendingAttachments: [],
    }),

  reloadMessages: async () => {
    const self = selfId();
    const peerId = get().activePeerId;
    if (!self || !peerId) return;
    set({ loadingMessages: true });
    try {
      const page = await listMessages(self, chatIdOf(peerId, self));
      // Drop a stale response if the user switched threads meanwhile.
      if (get().activePeerId === peerId) set({ messages: page.messages, error: null });
    } catch (err) {
      if (get().activePeerId === peerId) set({ error: describe(err) });
    } finally {
      set({ loadingMessages: false });
    }
  },

  attach: async (file) => {
    const self = selfId();
    const peerId = get().activePeerId;
    if (!self || !peerId) return false;

    // Snapshot the thread: a switch while the upload is in flight must not
    // drop the result into someone else's composer.
    const chatId = chatIdOf(peerId, self);
    set((s) => ({ uploading: [...s.uploading, file.name], error: null }));
    try {
      const attachment = await uploadChatAttachment(self, chatId, file);
      if (get().activePeerId !== peerId) return false;
      set((s) => ({ pendingAttachments: [...s.pendingAttachments, attachment] }));
      return true;
    } catch (err) {
      set({ error: describe(err) });
      return false;
    } finally {
      set((s) => ({ uploading: s.uploading.filter((n) => n !== file.name) }));
    }
  },

  removePendingAttachment: (id) =>
    set((s) => ({
      pendingAttachments: s.pendingAttachments.filter((a) => a.id !== id),
    })),

  send: async (body, attachmentIds) => {
    const self = selfId();
    const peerId = get().activePeerId;
    const ids = attachmentIds ?? get().pendingAttachments.map((a) => a.id);
    const text = body.trim();
    if (!self || !peerId || (!text && ids.length === 0)) return false;

    set({ sending: true });
    try {
      const sent = await sendChatMessage(
        self,
        chatIdOf(peerId, self),
        text,
        ids,
      );
      if (get().activePeerId === peerId) {
        set((s) => ({
          messages: [...s.messages, sent],
          error: null,
          // Only the set that actually went out is dropped; anything the
          // user attached during the round trip stays queued.
          pendingAttachments: s.pendingAttachments.filter(
            (a) => !ids.includes(a.id),
          ),
        }));
      }
      // The first message of a conversation also *creates* it, so refetch
      // rather than patching a row that may not exist yet.
      void get().refreshChats();
      return true;
    } catch (err) {
      set({ error: describe(err) });
      return false;
    } finally {
      set({ sending: false });
    }
  },

  reset: () => set({ ...EMPTY }),

  startPolling: () => {
    pollRefs += 1;
    if (pollTimer == null) {
      pollTimer = setInterval(() => {
        const current = get();
        void current.refreshChats();
        if (current.activePeerId) void current.reloadMessages();
      }, POLL_MS);
    }

    let released = false;
    return () => {
      // Guard the double-release a React 18 double-invoked effect would cause.
      if (released) return;
      released = true;
      pollRefs -= 1;
      if (pollRefs <= 0 && pollTimer != null) {
        clearInterval(pollTimer);
        pollTimer = null;
        pollRefs = 0;
      }
    };
  },
}));
