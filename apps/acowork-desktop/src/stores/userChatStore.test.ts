/**
 * Unit tests for the user↔user inbox store (ADR-076 §决策 8).
 *
 * The inbox is the only path where a human-authored message reaches the
 * wire, so pin the three things that would silently corrupt history:
 *
 *   1. the canonical `min__max` chat id — a flipped order would file the
 *      same pair under a second conversation and split the thread,
 *   2. the send path trusting the Gateway's echo (never a locally minted
 *      message — the server owns `ts` and `from`),
 *   3. the read receipt clearing the badge, and a failed send *keeping*
 *      the draft instead of losing the user's text.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../lib/config", () => ({ getGatewayUrl: () => "http://gw.test" }));
vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));

import { useAuthStore } from "./authStore";
import { chatIdOf, useUserChatStore } from "./userChatStore";
import type { UserAccount } from "../lib/types";

/** Deliberately the *larger* id, so a flipped pair would be caught. */
const SELF = "bbbbbbbb-0000-4000-8000-000000000002";
const PEER = "aaaaaaaa-0000-4000-8000-000000000001";
const CHAT = `${PEER}__${SELF}`;

const self: UserAccount = {
  user_id: SELF,
  username: "bob",
  display_name: "Bob",
  role: "admin",
  language: "en",
  timezone: "UTC",
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

const IMAGE = {
  id: "11111111-0000-4000-8000-000000000001",
  filename: "shot.png",
  mime: "image/png",
  size: 2048,
};
const DOC = {
  id: "22222222-0000-4000-8000-000000000002",
  filename: "spec.pdf",
  mime: "application/pdf",
  size: 4096,
};

function file(name: string): File {
  return new File([new Uint8Array([1, 2, 3])], name, { type: "image/png" });
}

function json(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as unknown as Response;
}

interface Route {
  method?: string;
  suffix: string;
  make: (init?: RequestInit) => Response;
}

/** Route-aware fetch stub so each test can assert the exact URL shape. */
function stubFetch(routes: Route[]) {
  const spy = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    for (const route of routes) {
      if (!url.includes(route.suffix)) continue;
      if (route.method && route.method !== method) continue;
      return route.make(init);
    }
    throw new Error(`unexpected fetch: ${method} ${url}`);
  });
  globalThis.fetch = spy as unknown as typeof fetch;
  return spy;
}

function summary(unread: number) {
  return {
    chat_id: CHAT,
    peer_user_id: PEER,
    peer_display_name: "Alice",
    last_active_at: 100,
    last_message_preview: "hello",
    unread_count: unread,
  };
}

beforeEach(() => {
  localStorage.clear();
  useUserChatStore.getState().reset();
  useAuthStore.setState({
    mode: "multi_user",
    status: "logged_in",
    account: self,
    accessToken: "at",
  });
});

describe("chatIdOf", () => {
  it("is order-independent and byte-sorted", () => {
    expect(chatIdOf(SELF, PEER)).toBe(CHAT);
    expect(chatIdOf(PEER, SELF)).toBe(CHAT);
  });
});

describe("useUserChatStore", () => {
  it("loads the signed-in user's conversations", async () => {
    const spy = stubFetch([{ suffix: "/chats", make: () => json({ chats: [summary(2)] }) }]);

    await useUserChatStore.getState().refreshChats();

    expect(useUserChatStore.getState().chats).toHaveLength(1);
    expect(useUserChatStore.getState().chats[0].unread_count).toBe(2);
    expect(String(spy.mock.calls[0][0])).toBe(`http://gw.test/api/users/${SELF}/chats`);
  });

  it("opens a thread, posts the read receipt and clears the badge", async () => {
    const spy = stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({
            chat_id: CHAT,
            messages: [{ ts: 10, from: PEER, kind: "text", body: "hi" }],
            total: 1,
            offset: 0,
            limit: 50,
          }),
      },
      { suffix: "/read", method: "POST", make: () => json(null, 204) },
      { suffix: "/chats", make: () => json({ chats: [summary(3)] }) },
    ]);

    await useUserChatStore.getState().refreshChats();
    await useUserChatStore.getState().openChat(PEER);

    const state = useUserChatStore.getState();
    expect(state.activePeerId).toBe(PEER);
    expect(state.messages.map((m) => m.body)).toEqual(["hi"]);
    expect(state.chats[0].unread_count).toBe(0);

    const readCall = spy.mock.calls.find(([, init]) => init?.method === "POST");
    expect(String(readCall?.[0])).toBe(
      `http://gw.test/api/users/${SELF}/chats/${CHAT}/read`,
    );
  });

  it("keeps the Gateway's echo of a sent message and refetches the list", async () => {
    const spy = stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      {
        suffix: "/messages",
        method: "POST",
        make: (init) => {
          const sent = JSON.parse(String(init?.body)) as { body: string };
          // The server owns ts/from/kind — the store must not mint them.
          return json({ ts: 500, from: SELF, kind: "text", body: sent.body }, 201);
        },
      },
      // Last: `/api/users/{id}/chats/{chat}/messages` also contains "/chats".
      { suffix: "/chats", make: () => json({ chats: [summary(0)] }) },
    ]);

    await useUserChatStore.getState().refreshChats();
    await useUserChatStore.getState().openChat(PEER);
    const ok = await useUserChatStore.getState().send("  hello alice  ");

    expect(ok).toBe(true);
    const { messages } = useUserChatStore.getState();
    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({ ts: 500, from: SELF, body: "hello alice" });

    await vi.waitFor(() => {
      const listCalls = spy.mock.calls.filter(
        ([url, init]) => String(url).endsWith("/chats") && init?.method === undefined,
      );
      expect(listCalls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it("reports a failed send and keeps the thread intact", async () => {
    stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      {
        suffix: "/messages",
        method: "POST",
        make: () => json({ error: "recipient account not found" }, 404),
      },
      { suffix: "/chats", make: () => json({ chats: [] }) },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    const ok = await useUserChatStore.getState().send("hello?");

    expect(ok).toBe(false);
    expect(useUserChatStore.getState().error).toBe("recipient account not found");
    expect(useUserChatStore.getState().messages).toHaveLength(0);
    expect(useUserChatStore.getState().sending).toBe(false);
  });

  it("does nothing when the account system is off", async () => {
    const spy = stubFetch([]);
    useAuthStore.setState({ mode: "local", status: "disabled", account: null });

    await useUserChatStore.getState().refreshChats();

    expect(spy).not.toHaveBeenCalled();
    expect(useUserChatStore.getState().chats).toHaveLength(0);
  });

  it("uploads a file to the conversation's own files route", async () => {
    const spy = stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      { suffix: "/files", method: "POST", make: () => json(IMAGE, 201) },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    const ok = await useUserChatStore.getState().attach(file("shot.png"));

    expect(ok).toBe(true);
    expect(useUserChatStore.getState().pendingAttachments).toEqual([IMAGE]);
    expect(useUserChatStore.getState().uploading).toEqual([]);

    const call = spy.mock.calls.find(
      ([url, init]) => String(url).endsWith("/files") && init?.method === "POST",
    );
    expect(String(call?.[0])).toBe(
      `http://gw.test/api/users/${SELF}/chats/${CHAT}/files`,
    );
    // FormData sets its own boundary; setting Content-Type by hand breaks it.
    // (`authFetch` will add Authorization, which is fine — that is its job.)
    expect(call?.[1]?.body).toBeInstanceOf(FormData);
    expect(
      (call?.[1]?.headers as Record<string, string> | undefined)?.[
        "Content-Type"
      ],
    ).toBeUndefined();
  });

  it("sends pending attachments by id and only clears what went out", async () => {
    const spy = stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      { suffix: "/files", method: "POST", make: () => json(IMAGE, 201) },
      {
        suffix: "/messages",
        method: "POST",
        make: () =>
          json(
            {
              ts: 900,
              from: SELF,
              kind: "image",
              body: "",
              // The Gateway echoes back its own record, not what we sent.
              attachments: [IMAGE],
            },
            201,
          ),
      },
      { suffix: "/chats", make: () => json({ chats: [] }) },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    await useUserChatStore.getState().attach(file("shot.png"));
    // An image with no caption is a message.
    const ok = await useUserChatStore.getState().send("   ");

    expect(ok).toBe(true);
    expect(useUserChatStore.getState().pendingAttachments).toEqual([]);
    expect(useUserChatStore.getState().messages[0].attachments).toEqual([IMAGE]);

    const post = spy.mock.calls.find(
      ([url, init]) =>
        String(url).endsWith("/messages") && init?.method === "POST",
    );
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      body: "",
      attachments: [IMAGE.id],
    });
  });

  it("keeps attachments queued when the send is refused", async () => {
    stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      { suffix: "/files", method: "POST", make: () => json(DOC, 201) },
      {
        suffix: "/messages",
        method: "POST",
        make: () => json({ error: "attachment is empty" }, 422),
      },
      { suffix: "/chats", make: () => json({ chats: [] }) },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    await useUserChatStore.getState().attach(file("spec.pdf"));
    const ok = await useUserChatStore.getState().send("here");

    expect(ok).toBe(false);
    // Uploading cost the user a round trip; a refused send must not drop it.
    expect(useUserChatStore.getState().pendingAttachments).toEqual([DOC]);
  });

  it("drops queued attachments when the thread is switched", async () => {
    stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      { suffix: "/files", method: "POST", make: () => json(IMAGE, 201) },
      { suffix: "/read", method: "POST", make: () => json(null, 204) },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    await useUserChatStore.getState().attach(file("shot.png"));
    expect(useUserChatStore.getState().pendingAttachments).toHaveLength(1);

    await useUserChatStore.getState().openChat("cccccccc-0000-4000-8000-000000000003");
    // They belong to the peer that was open, not to the composer in general.
    expect(useUserChatStore.getState().pendingAttachments).toEqual([]);
  });

  it("surfaces a rejected upload without queueing it", async () => {
    stubFetch([
      {
        suffix: "/messages",
        method: "GET",
        make: () =>
          json({ chat_id: CHAT, messages: [], total: 0, offset: 0, limit: 50 }),
      },
      {
        suffix: "/files",
        method: "POST",
        make: () => json({ error: "attachment exceeds 26214400 bytes" }, 413),
      },
    ]);

    await useUserChatStore.getState().openChat(PEER);
    const ok = await useUserChatStore.getState().attach(file("huge.png"));

    expect(ok).toBe(false);
    expect(useUserChatStore.getState().pendingAttachments).toEqual([]);
    expect(useUserChatStore.getState().error).toContain("exceeds");
    expect(useUserChatStore.getState().uploading).toEqual([]);
  });
});

describe("startPolling", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("shares a single loop across callers and stops with the last release", async () => {
    vi.useFakeTimers();
    const spy = stubFetch([
      { suffix: "/chats", make: () => json({ chats: [summary(1)] }) },
    ]);
    const store = useUserChatStore.getState();

    // The nav badge and the open inbox both want updates.
    const releaseBadge = store.startPolling();
    const releaseView = store.startPolling();

    await vi.advanceTimersByTimeAsync(5000);
    expect(spy).toHaveBeenCalledTimes(1); // one interval, not two

    releaseBadge();
    await vi.advanceTimersByTimeAsync(5000);
    expect(spy).toHaveBeenCalledTimes(2); // the view still holds it

    releaseView();
    await vi.advanceTimersByTimeAsync(5000);
    expect(spy).toHaveBeenCalledTimes(2); // nothing left to poll for
  });
});
