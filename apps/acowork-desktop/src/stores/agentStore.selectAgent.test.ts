/**
 * ADR-076 §决策 4 regression — `selectAgent` must not strand the chat panel
 * on the "Loading session…" spinner.
 *
 * Symptom (multi-user): account A opens a session and owns it — an owned
 * session is private from birth (and can be marked private explicitly).
 * Account B switches in. `GET /latest-session` reads an **agent-wide** cache,
 * so it names A's session and answers 404 (a private session is neither
 * readable nor writable for a non-owner; the Runtime refuses to leak the id).
 * `selectAgent` used to `return` on that 404 with no fallback, leaving
 * `activeSessionId` null forever → ChatPanel renders "Loading session…"
 * indefinitely.
 *
 * Fix: fall back to the account's own scope-filtered list (its newest
 * readable session), or create a fresh untitled session when it has none —
 * the same rule `initSessionForAgent` in lib/agent-start.ts already applies.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
    invoke: () => Promise.reject(new Error("not used in this test")),
}));

vi.mock("@tauri-apps/api/event", () => ({
    listen: () => Promise.resolve(() => {}),
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
import type { AgentInfo } from "./agentStore";
import type { SessionInfo } from "../lib/types";

const AGENT_ID = "com.acowork.architect";
const INSTANCE_ID = "b7f0c6c2-4f10-4f5e-9a1e-3d8e9f2a1c33";
const SID_MINE = "sess-mine";

interface FetchCall {
    url: string;
    method: string;
}

let fetchCalls: FetchCall[] = [];
/** What the scope-filtered `/sessions` list returns for this account. */
let listSessions: SessionInfo[] = [];

function makeMeta(): AgentInfo {
    return {
        agent_id: AGENT_ID,
        instance_id: INSTANCE_ID,
        name: "Architect",
        version: "1.0.0",
        avatar: null,
        builtin_avatar: null,
        display_name: null,
        role: null,
        alive: true,
        lifecycle: "sessions_ready",
        debug_state: "disabled",
        debug_port: null,
        workspace: "",
        workspace_config_json: null,
        current_embed_dim: null,
        migration: null,
        started_at: "2026-01-01T00:00:00Z",
        last_interaction_at: "2026-01-01T00:00:00Z",
    };
}

function makeSession(overrides: Partial<SessionInfo> = {}): SessionInfo {
    return {
        session_id: SID_MINE,
        created_at: "2026-02-01T00:00:00Z",
        last_active_at: "2026-02-01T00:00:00Z",
        message_count: 3,
        title: "Mine",
        can_write: true,
        ...overrides,
    };
}

function seedAgent(sessions: SessionInfo[]) {
    useAgentStore.setState({
        agents: {
            [INSTANCE_ID]: {
                meta: makeMeta(),
                profile: {} as never,
                sessions,
                sessionTitle: sessions[0]?.title ?? undefined,
                pagination: {
                    currentPage: 1,
                    totalPages: 1,
                    totalCount: sessions.length,
                    pageSize: 20,
                },
                isLoading: false,
                agentTokenTotals: null,
            },
        },
        selectedAgentId: INSTANCE_ID,
    });
}

/** Poll until `check()` is true (real timers), else fail. */
async function waitFor(check: () => boolean, timeoutMs = 2000): Promise<void> {
    const start = Date.now();
    while (!check()) {
        if (Date.now() - start > timeoutMs) throw new Error("waitFor timed out");
        await new Promise((r) => setTimeout(r, 10));
    }
}

beforeEach(() => {
    fetchCalls = [];
    listSessions = [];

    vi.stubGlobal(
        "fetch",
        vi.fn((url: string, init?: { method?: string; body?: string }) => {
            const u = String(url);
            const method = (init?.method ?? "GET").toUpperCase();
            fetchCalls.push({ url: u, method });
            const reply = (body: unknown, status = 200) =>
                Promise.resolve({
                    ok: status >= 200 && status < 300,
                    status,
                    text: () => Promise.resolve(status >= 300 ? "not found" : ""),
                    json: () => Promise.resolve(body),
                });

            // ADR-076: the agent-wide cache names a session we may not read.
            if (u.includes("/latest-session")) return reply({}, 404);
            if (u.includes("/sessions?")) {
                return reply({
                    sessions: listSessions,
                    total_count: listSessions.length,
                    total_pages: 1,
                });
            }
            if (method === "POST" && /\/sessions$/.test(u)) {
                return reply({ session_id: "brand-new" }, 201);
            }
            return reply({});
        }),
    );

    useAgentStore.setState({ agents: {}, selectedAgentId: null, loading: false, error: null });
    useChatStore.setState({ agentStates: {} });
});

afterEach(() => {
    vi.unstubAllGlobals();
});

describe("selectAgent — agent-wide latest session is not ours (ADR-076)", () => {
    it("opens this account's newest readable session when /latest-session 404s", async () => {
        listSessions = [makeSession()];
        seedAgent([]);

        useAgentStore.getState().selectAgent(INSTANCE_ID);

        await waitFor(
            () => useChatStore.getState().getActiveSessionId(INSTANCE_ID) === SID_MINE,
        );

        // The account's own list was consulted …
        expect(fetchCalls.some((c) => c.url.includes("/sessions?"))).toBe(true);
        // … and the newest readable session was opened.
        expect(useChatStore.getState().getOpenSessionIds(INSTANCE_ID)).toContain(SID_MINE);
        // No spurious session was created.
        expect(
            fetchCalls.some((c) => c.method === "POST" && /\/sessions$/.test(c.url)),
        ).toBe(false);
    });

    it("creates a fresh untitled session when the account has none", async () => {
        listSessions = [];
        seedAgent([]);

        useAgentStore.getState().selectAgent(INSTANCE_ID);

        await waitFor(() =>
            fetchCalls.some((c) => c.method === "POST" && /\/sessions$/.test(c.url)),
        );

        // Nothing was opened — activation rides the `session_created` MQTT
        // event, which never arrives in this unit test.
        expect(useChatStore.getState().getActiveSessionId(INSTANCE_ID)).toBeNull();
    });
});
