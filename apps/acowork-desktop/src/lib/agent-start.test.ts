/**
 * Regression coverage for the cold-start `initSessionForAgent` retry.
 *
 * Symptom: after starting an agent, the SessionTabBar above the chat
 * area renders "未命名" (Untitled) even though the AgentList sidebar
 * shows the correct title.  Switching to another agent (or opening the
 * session list dropdown, which triggers `fetchSessions`) re-renders
 * the tab with the correct title.
 *
 * Root cause: on cold start the Runtime's `/latest-session` endpoint
 * resolves quickly (it reads an in-memory cache), but `/sessions` is
 * slower because it scans disk and may 503 / return `[]` for the
 * brief boot window.  `initSessionForAgent` only retries
 * `fetchLatestSession`; the single `fetchSessions` call afterwards
 * can land during that window and leave `agents[id].sessions = []`,
 * while `sessionTitle` was already populated by the successful
 * `fetchLatestSession`.  The sidebar reads `sessionTitle` (correct)
 * and the SessionTabBar reads `sessions[i].title` (missing → fallback
 * "未命名").
 *
 * Fix: apply the same retry loop to `fetchSessions` so it keeps
 * calling until `sessions[]` actually contains the latest session.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const { mockStartAgent, mockWaitForAgentReady, mockFetchLatestSession,
    mockFetchSessions, mockCreateSession, mockEnsureLatestInCache, mockOpenSession,
    mockFetchWorkspaces, mockEmitAgentConfigRefresh } = vi.hoisted(() => ({
        mockStartAgent: vi.fn(),
        mockWaitForAgentReady: vi.fn(),
        mockFetchLatestSession: vi.fn(),
        mockFetchSessions: vi.fn(),
        mockCreateSession: vi.fn(),
        mockEnsureLatestInCache: vi.fn(),
        mockOpenSession: vi.fn(),
        mockFetchWorkspaces: vi.fn(),
        mockEmitAgentConfigRefresh: vi.fn(),
    }));

// Mock the agent store with controllable state. The real store wires
// `set` / `get` callbacks, but here we just need to verify the
// orchestrator's retry behaviour — so the agents map is a plain object
// the tests can poke.
let mockAgentsState: Record<string, {
    sessions: Array<{ session_id: string; title: string | null }>;
}> = {};

vi.mock("../stores/agentStore", () => ({
    useAgentStore: {
        getState: () => ({
            startAgent: mockStartAgent,
            waitForAgentReady: mockWaitForAgentReady,
            fetchLatestSession: mockFetchLatestSession,
            fetchSessions: mockFetchSessions,
            createSession: mockCreateSession,
            agents: mockAgentsState,
        }),
    },
}));

vi.mock("../stores/chatStore", () => ({
    useChatStore: {
        getState: () => ({
            ensureLatestInCache: mockEnsureLatestInCache,
            openSession: mockOpenSession,
        }),
    },
}));

vi.mock("../stores/workspaceStore", () => ({
    useWorkspaceStore: {
        getState: () => ({
            fetchWorkspaces: mockFetchWorkspaces,
        }),
    },
}));

vi.mock("./refresh", () => ({
    emitAgentConfigRefresh: mockEmitAgentConfigRefresh,
}));

vi.mock("@tauri-apps/api/core", () => ({
    invoke: () => Promise.reject(new Error("not used in this test")),
}));

vi.mock("../lib/logger", () => ({
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
    setLevel: () => {},
    getLevel: () => "off" as const,
}));

// ── SUT ──────────────────────────────────────────────────────────────────

import { startAgentAndSyncUI } from "./agent-start";

const AGENT_ID = "com.acowork.architect";
const SESSION_ID = "20260901_120000_aaaaaa";

beforeEach(() => {
    mockAgentsState = {};
    mockStartAgent.mockReset();
    mockWaitForAgentReady.mockReset();
    mockFetchLatestSession.mockReset();
    mockFetchSessions.mockReset();
    mockCreateSession.mockReset();
    mockEnsureLatestInCache.mockReset();
    mockOpenSession.mockReset();
    mockFetchWorkspaces.mockReset();
    mockEmitAgentConfigRefresh.mockReset();

    mockStartAgent.mockResolvedValue(undefined);
    mockWaitForAgentReady.mockResolvedValue(undefined);
    mockCreateSession.mockResolvedValue(undefined);
    mockEnsureLatestInCache.mockResolvedValue(undefined);
    mockOpenSession.mockResolvedValue(undefined);
    mockFetchWorkspaces.mockResolvedValue(undefined);
    mockEmitAgentConfigRefresh.mockReturnValue(undefined);
});

describe("initSessionForAgent — fetchSessions retry", () => {
    it("retries fetchSessions until the latest session appears in agents[id].sessions", async () => {
        // fetchLatestSession resolves immediately with the target session.
        mockFetchLatestSession.mockResolvedValue({
            status: "ok",
            session_id: SESSION_ID,
            title: "Hello world",
        });

        // Simulate the cold-start race: the first fetchSessions call
        // lands while the disk scan is still warming up and returns
        // without populating sessions[]. The second call lands after
        // the scan has completed and writes the real session list.
        // (Test stays under the 5s vitest default budget: 1× 1s sleep.)
        mockFetchSessions
            .mockImplementationOnce(() => {
                mockAgentsState[AGENT_ID] = { sessions: [] };
            })
            .mockImplementationOnce(() => {
                mockAgentsState[AGENT_ID] = {
                    sessions: [{ session_id: SESSION_ID, title: "Hello world" }],
                };
            });

        await startAgentAndSyncUI(AGENT_ID);

        // Without the retry, fetchSessions would be called exactly once
        // and the SessionTabBar would mount with sessions = [] → "未命名".
        // With the fix, the orchestrator keeps calling fetchSessions
        // until agents[id].sessions contains the latest session, so the
        // tab title is correct on first paint.
        expect(mockFetchSessions).toHaveBeenCalledTimes(2);

        // openSession must run AFTER the session list is populated, so
        // SessionTabBar mounts with the title already in place.
        const openOrder = mockOpenSession.mock.invocationCallOrder[0]!;
        const lastFetchOrder =
            mockFetchSessions.mock.invocationCallOrder[1]!;
        expect(openOrder).toBeGreaterThan(lastFetchOrder);
    });
});

describe("initSessionForAgent — no readable latest session (ADR-076)", () => {
    /**
     * `/latest-session` answers from an agent-wide cache, so under
     * `multi_user` it 404s as soon as a second account has used this
     * agent: the cached newest session belongs to them, and the Runtime
     * refuses to name it rather than leak the id.
     *
     * Retrying cannot fix that, and ten seconds of retrying before
     * giving up is a bad first paint. The caller's own scope-filtered
     * list can answer immediately, so it is consulted inside the loop.
     */
    it("opens the caller's newest readable session instead of retrying", async () => {
        mockFetchLatestSession.mockResolvedValue({ status: "no_session" });
        mockFetchSessions.mockImplementation(() => {
            mockAgentsState[AGENT_ID] = {
                sessions: [{ session_id: SESSION_ID, title: "Mine" }],
            };
        });

        await startAgentAndSyncUI(AGENT_ID);

        expect(mockOpenSession).toHaveBeenCalledWith(AGENT_ID, SESSION_ID);
        // No session was invented, and no retry sleep was spent.
        expect(mockCreateSession).not.toHaveBeenCalled();
        expect(mockFetchLatestSession).toHaveBeenCalledTimes(1);
    });

    /**
     * The other half of the same state: the list is empty too, and the
     * startup scan had its full budget to say otherwise. That is the
     * normal first run under `multi_user` (the agent's cold-start
     * session has no owner, so it is nobody's) — the account needs a
     * session of its own rather than a blank chat panel.
     */
    it("creates a session when the account genuinely has none", async () => {
        vi.useFakeTimers();
        try {
            mockFetchLatestSession.mockResolvedValue({ status: "no_session" });
            mockFetchSessions.mockImplementation(() => {
                mockAgentsState[AGENT_ID] = { sessions: [] };
            });

            const started = startAgentAndSyncUI(AGENT_ID);
            // Drain the 10-retry budget (9 × 1s sleeps).
            await vi.advanceTimersByTimeAsync(10_000);
            await started;

            expect(mockCreateSession).toHaveBeenCalledWith(AGENT_ID);
            expect(mockOpenSession).not.toHaveBeenCalled();
        } finally {
            vi.useRealTimers();
        }
    });

    /**
     * ADR-085 D4/D7: `unavailable` (503 boot window / network) is NOT
     * "zero sessions". The orchestrator must exhaust its retry budget
     * and then fail — never invent a session that races the Runtime's
     * own startup (the duplicate-session bug this ADR removes).
     */
    it("does not create a session when the state is unavailable", async () => {
        vi.useFakeTimers();
        try {
            mockFetchLatestSession.mockResolvedValue({ status: "unavailable" });
            mockFetchSessions.mockImplementation(() => {
                mockAgentsState[AGENT_ID] = { sessions: [] };
            });

            const started = startAgentAndSyncUI(AGENT_ID);
            // Attach the rejection handler BEFORE advancing timers so the
            // rejection is never "unhandled" mid-drain.
            const assertion = expect(started).rejects.toThrow(/unavailable/);
            await vi.advanceTimersByTimeAsync(15_000);
            // startAgentAndSyncUI wraps initSessionForAgent in try/catch
            // and rethrows — expect the rejection, not a silent create.
            await assertion;

            expect(mockCreateSession).not.toHaveBeenCalled();
            expect(mockOpenSession).not.toHaveBeenCalled();
        } finally {
            vi.useRealTimers();
        }
    });
});
