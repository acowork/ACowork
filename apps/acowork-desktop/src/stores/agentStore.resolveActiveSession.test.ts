/**
 * Self-check for `agentStore.resolveActiveSession` — the single-flight
 * "decide and open the active session" resolver.
 *
 * Before the session-load consolidation, the start orchestrator
 * (`lib/agent-start.ts`), `selectAgent` and the `fetchAgents` reload
 * re-bind each ran their own copy of resolve → open, so even a single
 * agent start stacked up to three concurrent chains that aborted each
 * other's message loads. The resolver now owns the whole chain behind
 * one in-flight promise per agent:
 *
 *   - tri-state from `/latest-session`; `unavailable` must NOT fall
 *     through to `createSession` (that is the duplicate-session race),
 *   - ADR-076 §decision 4 fallback to this account's own list when the
 *     agent-wide latest session is not ours (`no_session`),
 *   - `createSession` only when the account has no rows at all,
 *   - `openSession` + background list refresh otherwise.
 *
 * These tests drive the REAL store with `fetchLatestSession` /
 * `fetchSessions` / `createSession` / `openSession` replaced via
 * `setState`, and assert the terminal result and call counts — including
 * the single-flight coalescing.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const mockInvoke = vi.fn().mockResolvedValue(null);
vi.mock("@tauri-apps/api/core", () => ({
    invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("../lib/logger", () => ({
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
    setLevel: () => {},
    getLevel: () => "off" as const,
}));

vi.mock("../lib/profileStore", () => ({
    loadAllProfiles: () => ({}),
    loadProfile: () => null,
    saveProfile: () => {},
    DEFAULT_PROFILE: {
        language: "zh-CN",
        timezone: "Asia/Shanghai",
        model_override: null,
        reasoning_effort: null,
        auto_approve_tools: [],
        yolo_mode: false,
    },
}));

import { useAgentStore } from "./agentStore";
import { useChatStore } from "./chatStore";

const AGENT = "com.acowork.architect";
const SESSION_FROM_LIST = "sess-from-account-list";
const SESSION_FROM_LATEST = "sess-from-latest";

// ── controllable doubles ────────────────────────────────────────────────

const mockFetchLatest = vi.fn();
const mockFetchSessions = vi.fn();
const mockCreateSession = vi.fn();
const mockOpenSession = vi.fn();

const original = {
    fetchLatestSession: useAgentStore.getState().fetchLatestSession,
    fetchSessions: useAgentStore.getState().fetchSessions,
    createSession: useAgentStore.getState().createSession,
    openSession: useChatStore.getState().openSession,
};

/** Seed `agents[AGENT].sessions` — what a real `fetchSessions` would leave behind. */
function seedSessions(sessionIds: string[]): void {
    useAgentStore.setState({
        agents: {
            [AGENT]: {
                meta: null,
                profile: {},
                sessions: sessionIds.map((id) => ({ session_id: id })),
                sessionTitle: undefined,
                pagination: { currentPage: 1, totalPages: 1, totalCount: sessionIds.length, pageSize: 20 },
                isLoading: false,
            },
        },
    } as never);
}

/** Seed an existing active session on the chat store (the `noop` gate). */
function seedActiveSession(sessionId: string): void {
    useChatStore.setState((state) => ({
        agentStates: {
            ...state.agentStates,
            [AGENT]: {
                sessionStates: {},
                activeSessionId: sessionId,
                openSessionIds: [sessionId],
                lastLoadedSessionId: null,
                isSessionInitLoading: false,
                preferredModel: null,
                preferredProvider: null,
                llmAvailability: "unspecified",
            },
        },
    }) as never);
}

beforeEach(() => {
    mockFetchLatest.mockReset();
    mockFetchSessions.mockReset().mockResolvedValue(undefined);
    mockCreateSession.mockReset().mockResolvedValue(undefined);
    mockOpenSession.mockReset().mockResolvedValue(undefined);

    useAgentStore.setState({
        agents: {},
        fetchLatestSession: mockFetchLatest,
        fetchSessions: mockFetchSessions,
        createSession: mockCreateSession,
    } as never);
    useChatStore.setState({
        agentStates: {},
        openSession: mockOpenSession,
    } as never);
});

afterEach(() => {
    useAgentStore.setState({
        agents: {},
        fetchLatestSession: original.fetchLatestSession,
        fetchSessions: original.fetchSessions,
        createSession: original.createSession,
    } as never);
    useChatStore.setState({
        agentStates: {},
        openSession: original.openSession,
    } as never);
});

describe("agentStore.resolveActiveSession", () => {
    it("returns `noop` when an active session already exists (no fetch, no open)", async () => {
        seedActiveSession("sess-existing");

        const result = await useAgentStore.getState().resolveActiveSession(AGENT);

        expect(result).toBe("noop");
        expect(mockFetchLatest).not.toHaveBeenCalled();
        expect(mockOpenSession).not.toHaveBeenCalled();
        expect(mockCreateSession).not.toHaveBeenCalled();
    });

    it("returns `unavailable` while the runtime is booting — never creates a session", async () => {
        mockFetchLatest.mockResolvedValue({ status: "unavailable" });

        const result = await useAgentStore.getState().resolveActiveSession(AGENT);

        expect(result).toBe("unavailable");
        expect(mockFetchSessions).not.toHaveBeenCalled();
        expect(mockCreateSession).not.toHaveBeenCalled();
        expect(mockOpenSession).not.toHaveBeenCalled();
    });

    it("no_session → falls back to this account's list and opens the newest row", async () => {
        mockFetchLatest.mockResolvedValue({ status: "no_session" });
        // The real fetchSessions populates `agents[AGENT].sessions`; the
        // resolver reads sessions[0] right after awaiting it.
        mockFetchSessions.mockImplementation(async () => {
            seedSessions([SESSION_FROM_LIST]);
        });

        const result = await useAgentStore.getState().resolveActiveSession(AGENT);

        expect(result).toBe("opened");
        expect(mockOpenSession).toHaveBeenCalledTimes(1);
        expect(mockOpenSession).toHaveBeenCalledWith(AGENT, SESSION_FROM_LIST);
        expect(mockCreateSession).not.toHaveBeenCalled();
        // Fallback fetch + the background list refresh after open.
        expect(mockFetchSessions).toHaveBeenCalledTimes(2);
    });

    it("status ok → opens the latest session and refreshes the list", async () => {
        mockFetchLatest.mockResolvedValue({
            status: "ok",
            session_id: SESSION_FROM_LATEST,
            title: "Latest",
        });

        const result = await useAgentStore.getState().resolveActiveSession(AGENT);

        expect(result).toBe("opened");
        expect(mockOpenSession).toHaveBeenCalledWith(AGENT, SESSION_FROM_LATEST);
        // No fallback list fetch — only the background refresh.
        expect(mockFetchSessions).toHaveBeenCalledTimes(1);
    });

    it("creates a fresh session when the account has no rows at all", async () => {
        mockFetchLatest.mockResolvedValue({ status: "no_session" });
        // fetchSessions leaves the list empty → no fallback target.
        mockFetchSessions.mockResolvedValue(undefined);

        const result = await useAgentStore.getState().resolveActiveSession(AGENT);

        expect(result).toBe("created");
        expect(mockCreateSession).toHaveBeenCalledTimes(1);
        expect(mockCreateSession).toHaveBeenCalledWith(AGENT);
        // Activation rides the `session_created` event — nothing to open here.
        expect(mockOpenSession).not.toHaveBeenCalled();
    });

    it("single-flight: concurrent calls share one resolution", async () => {
        let release!: (value: unknown) => void;
        mockFetchLatest.mockImplementationOnce(
            () => new Promise((resolve) => { release = resolve; }),
        );

        const p1 = useAgentStore.getState().resolveActiveSession(AGENT);
        const p2 = useAgentStore.getState().resolveActiveSession(AGENT);
        // Let the shared chain reach the gated fetchLatestSession.
        await new Promise((r) => setTimeout(r, 0));

        release({ status: "ok", session_id: SESSION_FROM_LATEST, title: null });

        const [r1, r2] = await Promise.all([p1, p2]);
        expect(r1).toBe("opened");
        expect(r2).toBe("opened");
        expect(mockFetchLatest).toHaveBeenCalledTimes(1);
        expect(mockOpenSession).toHaveBeenCalledTimes(1);
    });
});
