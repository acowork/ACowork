/**
 * Regression: "switch away during a huge explore block → switch back shows
 * only the explore block, earlier messages gone".
 *
 * Root causes pinned here:
 *   1. clearSessionMessages() resets the cursor and flags the cache stale.
 *      A background record_complete must NOT write into that non-authoritative
 *      cache (it would bump messageLimit with offset pinned at 0).
 *   2. On the next window load, a stale cache must adopt the server cursor
 *      verbatim — never union it into a bogus `[0, total)` window.
 *   3. A tail page is measured in RAW entries, but the renderer folds a whole
 *      explore run into ONE block.  ensureLatestInCache must keep pulling
 *      older pages until the window starts on a turn boundary, otherwise the
 *      user message that opened the turn is never loaded.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useChatStore, handleMessageEvent } from "./chatStore";
import { useChatAdapterStore } from "../components/chat/chatAdapterStore";
import { foldMessages } from "../components/chat/messageFolder";
import type { ChatMessage } from "../lib/types";

const AGENT = "com.test.Agent";
const SESSION = "sess-test";

function tool(i: number): ChatMessage {
  return { id: `tool-${i}`, type: "tool_call", content: `call ${i}`, timestamp: 1000 + i };
}

/** Conversation of `total` raw entries: entry 0 is a user message, the rest
 *  are one giant explore run (tool_call). */
function servePage(url: string, total: number) {
  const entries = Array.from({ length: total }, (_, i) =>
    i === 0
      ? { id: "u-0", role: "user", content: "hi", ts: new Date(1000).toISOString() }
      : { id: `tool-${i}`, role: "tool_call", content: `call ${i}`, ts: new Date(1000 + i).toISOString() },
  );
  const params = new URL(url).searchParams;
  const limit = Number(params.get("limit") ?? 50);
  let offset: number;
  if (params.get("tail") === "true") {
    offset = Math.max(0, total - limit);
  } else {
    offset = Number(params.get("offset") ?? 0);
  }
  const slice = entries.slice(offset, offset + limit);
  return {
    ok: true,
    status: 200,
    json: () =>
      Promise.resolve({
        messages: slice,
        offset,
        limit: slice.length,
        total,
      }),
  };
}

function stubFetch(total: number) {
  vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(servePage(url, total))));
}

beforeEach(() => {
  useChatStore.setState((s) => ({ ...s, agentStates: {} }));
  useChatAdapterStore.setState({ sessions: {} });
});
afterEach(() => vi.unstubAllGlobals());

describe("huge explore block: pagination must not hide the pre-turn messages", () => {
  it("background record_complete into a cleared session is dropped (no cursor lie)", () => {
    useChatStore.setState((s) => ({
      ...s,
      agentStates: {
        ...s.agentStates,
        [AGENT]: {
          ...(s.agentStates[AGENT] ?? {}),
          activeSessionId: SESSION,
          sessionStates: { [SESSION]: { ...useChatStore.getState().getSessionState(AGENT, SESSION), messages: [], loadSequence: 0 } },
        },
      },
    }));
    useChatStore.getState().clearSessionMessages(AGENT, SESSION);

    handleMessageEvent(
      { type: "record_complete", session_id: SESSION, message_id: "a-1", role: "assistant", content: "x" },
      useChatStore.setState,
      useChatStore.getState,
      AGENT,
    );

    const ss = useChatStore.getState().getSessionState(AGENT, SESSION);
    expect(ss.messages).toEqual([]);
    expect(ss.messageOffset).toBe(0);
    expect(ss.messageLimit).toBe(0);
    expect(ss.messagesStale).toBe(true);
  });

  it("a stale load adopts the server cursor verbatim (hasOlder stays true)", async () => {
    // Sparse stale cache: only 250 tail records, but the real conversation
    // has 300 entries (head at offset 0 missing).
    useChatStore.setState((s) => ({
      ...s,
      agentStates: {
        ...s.agentStates,
        [AGENT]: {
          ...(s.agentStates[AGENT] ?? {}),
          activeSessionId: SESSION,
          sessionStates: {
            [SESSION]: {
              ...useChatStore.getState().getSessionState(AGENT, SESSION),
              messages: Array.from({ length: 250 }, (_, i) => tool(50 + i)),
              messageOffset: 0,
              messageLimit: 250,
              messageTotal: 250,
              messagesStale: true,
              loadSequence: 0,
            },
          },
        },
      },
    }));
    stubFetch(300);

    await useChatStore.getState().loadSessionMessages(AGENT, SESSION, undefined, 50);

    const ss = useChatStore.getState().getSessionState(AGENT, SESSION);
    // Cursor is the server's tail window, NOT a fabricated [0,300).
    expect(ss.messageOffset).toBe(250);
    expect(ss.messageLimit).toBe(50);
    expect(ss.messageTotal).toBe(300);
    // hasOlder === (offset > 0) → the older head is loadable.
    expect(ss.messageOffset).toBeGreaterThan(0);
    expect(ss.messagesStale).toBe(false);
  });

  it("ensureLatestInCache expands back to the turn boundary (loads the user message)", async () => {
    stubFetch(120); // [0] user, [1..119] one explore run

    await useChatStore.getState().ensureLatestInCache(AGENT, SESSION);

    const ss = useChatStore.getState().getSessionState(AGENT, SESSION);
    // The tail page alone would be entries [70,120) — all tool_call — which
    // folds to a single block.  After expansion the window reaches entry 0.
    expect(ss.messages[0]?.type).toBe("user");
    expect(ss.messageOffset).toBe(0);
  });

  it("50 raw tool entries fold to a single explore_group block (why expansion is required)", () => {
    const blocks = foldMessages(Array.from({ length: 50 }, (_, i) => tool(i)));
    expect(blocks).toHaveLength(1);
    expect(blocks[0].type).toBe("explore_group");
  });
});
