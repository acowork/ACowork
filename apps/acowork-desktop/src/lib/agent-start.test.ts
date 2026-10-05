/**
 * Regression coverage for the cold-start session-init orchestrator
 * (`startAgentAndSyncUI` → `initSessionForAgent`).
 *
 * Two layers of behaviour are pinned here:
 *
 * 1. Resolve retry — the Runtime answers `unavailable` until it reaches
 *    SESSIONS_READY; the orchestrator must retry the resolver (max 10
 *    attempts, 1s interval) and then FAIL rather than invent a session
 *    (ADR-085 D4/D7: never race the Runtime's own startup).
 *
 * 2. List population retry — on cold start the Runtime's
 *    `/latest-session` resolves quickly (it reads an in-memory cache),
 *    but `/sessions` is slower because it scans disk and may 503 /
 *    return `[]` for the brief boot window.  Without a retry the
 *    SessionTabBar mounts with `sessions = []` → "未命名" (Untitled)
 *    even though the sidebar title is correct.
 *
 * The resolution chain itself (latest-session tri-state → own-list
 * fallback → create) and the open (UI + backend activation + message
 * load) now live in `agentStore.resolveActiveSession` (single-flight) —
 * its own suite (`agentStore.resolveActiveSession.test.ts`) covers the
 * fallback / create branches. These tests pin the orchestrator's
 * contract with it: retry on `unavailable`, list population on
 * `noop` / `opened`, nothing on `created`, rejection on exhausted budget.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const { mockWaitForAgentReady, mockResolveActiveSession,
    mockFetchSessions, mockGetActiveSessionId,
    mockFetchWorkspaces, mockEmitAgentConfigRefresh } = vi.hoisted(() => ({
        mockWaitForAgentReady: vi.fn(),
        mockResolveActiveSession: vi.fn(),
        mockFetchSessions: vi.fn(),
        mockGetActiveSessionId: vi.fn(),
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
            waitForAgentReady: mockWaitForAgentReady,
            resolveActiveSession: mockResolveActiveSession,
            fetchSessions: mockFetchSessions,
            agents: mockAgentsState,
        }),
    },
}));

vi.mock("../stores/chatStore", () => ({
    useChatStore: {
        getState: () => ({
            getActiveSessionId: mockGetActiveSessionId,
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

vi.mock("../lib/refresh", () => ({
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
    mockWaitForAgentReady.mockReset();
    mockResolveActiveSession.mockReset();
    mockFetchSessions.mockReset();
    mockGetActiveSessionId.mockReset();
    mockFetchWorkspaces.mockReset();
    mockEmitAgentConfigRefresh.mockReset();

    mockWaitForAgentReady.mockResolvedValue(undefined);
    mockFetchWorkspaces.mockResolvedValue(undefined);
    mockEmitAgentConfigRefresh.mockReturnValue(undefined);
    mockGetActiveSessionId.mockReturnValue(null);
});

describe("initSessionForAgent — resolve retry", () => {
    it("retries resolveActiveSession while the result is unavailable", async () => {
        vi.useFakeTimers();
        try {
            mockResolveActiveSession
                .mockResolvedValueOnce("unavailable")
                .mockResolvedValueOnce("opened");
            mockGetActiveSessionId.mockReturnValue(SESSION_ID);
            mockFetchSessions.mockImplementation(() => {
                mockAgentsState[AGENT_ID] = {
                    sessions: [{ session_id: SESSION_ID, title: "Hello world" }],
                };
            });

            const started = startAgentAndSyncUI(AGENT_ID);
            // Drain one 1s retry sleep.
            await vi.advanceTimersByTimeAsync(2_000);
            await started;

            expect(mockResolveActiveSession).toHaveBeenCalledTimes(2);
            // Second resolve opened the session → one list population.
            expect(mockFetchSessions).toHaveBeenCalledTimes(1);
        } finally {
            vi.useRealTimers();
        }
    });
});

describe("initSessionForAgent — fetchSessions retry", () => {
    it("retries fetchSessions until the active session appears in agents[id].sessions", async () => {
        // The resolver opens the session immediately.
        mockResolveActiveSession.mockResolvedValue("opened");
        mockGetActiveSessionId.mockReturnValue(SESSION_ID);

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

        // The resolver (which owns openSession) must run BEFORE the list
        // population, so SessionTabBar mounts with the title already in
        // place — same ordering guarantee as the retired in-orchestrator
        // openSession call.
        const resolveOrder = mockResolveActiveSession.mock.invocationCallOrder[0]!;
        const lastFetchOrder = mockFetchSessions.mock.invocationCallOrder[1]!;
        expect(resolveOrder).toBeLessThan(lastFetchOrder);
    });

    it("still populates the list when the resolver reports noop", async () => {
        // `noop` (activeSessionId already set by a concurrent chain) is a
        // terminal success — the orchestrator's list population still
        // runs so the tab bar shows the right title.
        mockResolveActiveSession.mockResolvedValue("noop");
        mockGetActiveSessionId.mockReturnValue(SESSION_ID);
        mockFetchSessions.mockImplementation(() => {
            mockAgentsState[AGENT_ID] = {
                sessions: [{ session_id: SESSION_ID, title: "Hello world" }],
            };
        });

        await startAgentAndSyncUI(AGENT_ID);

        expect(mockFetchSessions).toHaveBeenCalledTimes(1);
    });
});

describe("initSessionForAgent — created outcome", () => {
    it("returns without touching the list when the resolver created a session", async () => {
        mockResolveActiveSession.mockResolvedValue("created");

        await startAgentAndSyncUI(AGENT_ID);

        // Activation rides the `session_created` MQTT event
        // (`activateNewlyCreatedSession`) — the orchestrator neither
        // opens nor list-populates anything itself.
        expect(mockFetchSessions).not.toHaveBeenCalled();
        expect(mockGetActiveSessionId).not.toHaveBeenCalled();
    });
});

describe("initSessionForAgent — unavailable", () => {
    it("does not invent a session when the state stays unavailable", async () => {
        vi.useFakeTimers();
        try {
            mockResolveActiveSession.mockResolvedValue("unavailable");

            const started = startAgentAndSyncUI(AGENT_ID);
            // Attach the rejection handler BEFORE advancing timers so the
            // rejection is never "unhandled" mid-drain.
            const assertion = expect(started).rejects.toThrow(/unavailable/);
            // Drain the 10-retry budget (9 × 1s sleeps).
            await vi.advanceTimersByTimeAsync(15_000);
            await assertion;

            expect(mockResolveActiveSession).toHaveBeenCalledTimes(10);
            expect(mockFetchSessions).not.toHaveBeenCalled();
            expect(mockGetActiveSessionId).not.toHaveBeenCalled();
        } finally {
            vi.useRealTimers();
        }
    });
});
