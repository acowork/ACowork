/**
 * Single-flight checks for `chatStore.openSession` and
 * `chatStore.ensureLatestInCache`.
 *
 * Regression context: multiple chains (the start orchestrator,
 * `selectAgent`'s resolver, session-tab clicks, ChatPanel effects,
 * StrictMode double mounts) could target the same session within the
 * same tick. Each open used to re-send `open_session` and restart
 * `loadSessionMessages`, aborting the previous in-flight load; each tail
 * reload bailed out on the `isLoadingMore` flag instead of waiting, so
 * its caller silently got nothing. Both are now single-flight:
 * concurrent callers share one promise, and a second caller waits for
 * the reload it asked about.
 *
 * The tests drive the REAL store. `loadSessionMessages` / `loadSession`
 * are replaced via `setState` so the load leg can be gated and counted,
 * and `fetch` is stubbed so the `/open` control request is observable.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../lib/logger", () => ({
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
    setLevel: () => {},
    getLevel: () => "off" as const,
}));

import { useChatStore } from "./chatStore";

const AGENT = "com.acowork.architect";
const S1 = "session-1";
const S2 = "session-2";

// ── fetch stub: only the `/open` control request matters here ──────────

let openRequests = 0;

const mockLoadMessages = vi.fn();
const mockLoadSession = vi.fn();

const original = {
    loadSessionMessages: useChatStore.getState().loadSessionMessages,
    loadSession: useChatStore.getState().loadSession,
};

beforeEach(() => {
    openRequests = 0;
    vi.stubGlobal("fetch", vi.fn(async (input: unknown) => {
        const url = String(input);
        if (url.includes("/open")) openRequests += 1;
        return {
            ok: true,
            status: 200,
            headers: { get: () => null },
            text: async () => "",
            json: async () => ({}),
        } as unknown as Response;
    }));

    mockLoadMessages.mockReset().mockResolvedValue(undefined);
    mockLoadSession.mockReset().mockResolvedValue(undefined);

    useChatStore.setState({
        agentStates: {},
        loadSessionMessages: mockLoadMessages,
        loadSession: mockLoadSession,
    } as never);
});

afterEach(() => {
    useChatStore.setState({
        agentStates: {},
        loadSessionMessages: original.loadSessionMessages,
        loadSession: original.loadSession,
    } as never);
    vi.unstubAllGlobals();
});

describe("chatStore.openSession — single-flight", () => {
    it("shares one in-flight open for the same (agentId, sessionId)", async () => {
        // Gate the load leg so the second call lands while the first is
        // still mid-open (fetch dispatched, load awaiting).
        let release!: () => void;
        mockLoadMessages.mockImplementationOnce(
            () => new Promise<undefined>((resolve) => { release = () => resolve(undefined); }),
        );

        const p1 = useChatStore.getState().openSession(AGENT, S1);
        await new Promise((r) => setTimeout(r, 0));
        const p2 = useChatStore.getState().openSession(AGENT, S1);
        release();
        await Promise.all([p1, p2]);

        // One `open_session` request, one message load, one config/state load.
        expect(openRequests).toBe(1);
        expect(mockLoadMessages).toHaveBeenCalledTimes(1);
        expect(mockLoadSession).toHaveBeenCalledTimes(1);
    });

    it("does not coalesce opens of different sessions", async () => {
        const p1 = useChatStore.getState().openSession(AGENT, S1);
        const p2 = useChatStore.getState().openSession(AGENT, S2);
        await Promise.all([p1, p2]);

        expect(openRequests).toBe(2);
        expect(mockLoadMessages).toHaveBeenCalledTimes(2);
        expect(mockLoadMessages.mock.calls.map((c) => c[1])).toEqual([S1, S2]);
    });

    it("releases the in-flight entry after completion (re-open hits the backend again)", async () => {
        await useChatStore.getState().openSession(AGENT, S1);
        await useChatStore.getState().openSession(AGENT, S1);

        expect(openRequests).toBe(2);
        expect(mockLoadMessages).toHaveBeenCalledTimes(2);
    });
});

describe("chatStore.ensureLatestInCache — single-flight", () => {
    it("a second caller waits for the in-flight reload instead of getting an empty hand", async () => {
        let release!: () => void;
        mockLoadMessages.mockImplementationOnce(
            () => new Promise<undefined>((resolve) => { release = () => resolve(undefined); }),
        );

        const p1 = useChatStore.getState().ensureLatestInCache(AGENT, S1);
        await new Promise((r) => setTimeout(r, 0));

        const p2 = useChatStore.getState().ensureLatestInCache(AGENT, S1);
        let p2Settled = false;
        void p2.then(() => { p2Settled = true; });
        await new Promise((r) => setTimeout(r, 0));
        // Still in flight — the second caller must NOT resolve early.
        expect(p2Settled).toBe(false);

        release();
        await Promise.all([p1, p2]);

        expect(p2Settled).toBe(true);
        // One shared reload (fresh cache → tail fetch passes no offset).
        expect(mockLoadMessages).toHaveBeenCalledTimes(1);
        expect(mockLoadMessages).toHaveBeenCalledWith(AGENT, S1, undefined, 50);
        // The UI flag is cleared by the shared promise's finally.
        expect(useChatStore.getState().getSessionState(AGENT, S1).isLoadingMore).toBe(false);
    });
});
