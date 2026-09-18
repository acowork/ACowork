/**
 * ADR-076 §决策 4 — session sharing ("public" read-only) frontend plumbing.
 *
 * Two behaviours, both about the *viewer* side of a shared session:
 *
 *   1. The visibility toggle (composer icon) is optimistic: it patches the
 *      session row, PUTs `/sessions/{sid}/visibility`, and rolls the row
 *      back + rethrows when the backend refuses (a non-owner's PUT is a
 *      404 — the toggle is disabled in the UI, this covers the race where
 *      the list data is stale).
 *
 *   2. Closing a tab for a session we only *view* (`can_write === false`)
 *      must NOT call `POST /close`: closing is write-gated precisely so a
 *      bystander cannot tear down the owner's session. Closing the tab is
 *      a purely local act for a viewer, so no request may leave.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Keep the Tauri invoke stub — agentStore imports it at module load.
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
const SID = "sess-shared";

interface FetchCall {
    url: string;
    method: string;
    body: unknown;
}

let fetchCalls: FetchCall[] = [];
let fetchStatus = 200;

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
        running: true,
        ready: true,
        connected: true,
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
        session_id: SID,
        created_at: "2026-01-01T00:00:00Z",
        last_active_at: "2026-01-01T00:00:00Z",
        message_count: 0,
        title: "Shared session",
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
                online: true,
                sleeping: false,
            },
        },
        selectedAgentId: INSTANCE_ID,
    });
}

function visibilityOf(): string | null | undefined {
    return useAgentStore
        .getState()
        .agents[INSTANCE_ID].sessions.find((s) => s.session_id === SID)?.visibility;
}

beforeEach(() => {
    fetchCalls = [];
    fetchStatus = 200;
    vi.stubGlobal(
        "fetch",
        vi.fn((url: string, init?: { method?: string; body?: string }) => {
            fetchCalls.push({
                url: String(url),
                method: init?.method ?? "GET",
                body: init?.body ? JSON.parse(init.body) : undefined,
            });
            return Promise.resolve({
                ok: fetchStatus >= 200 && fetchStatus < 300,
                status: fetchStatus,
                text: () => Promise.resolve(fetchStatus >= 300 ? "not found" : ""),
                json: () => Promise.resolve({}),
            });
        }),
    );
    useAgentStore.setState({ agents: {}, selectedAgentId: null, loading: false, error: null });
    useChatStore.setState({ agentStates: {} });
});

afterEach(() => {
    vi.unstubAllGlobals();
});

describe("setSessionVisibility — optimistic owner toggle", () => {
    it("flips the session row and PUTs the visibility endpoint", async () => {
        seedAgent([makeSession({ visibility: "public", can_write: true })]);

        await useAgentStore.getState().setSessionVisibility(INSTANCE_ID, SID, "private");

        expect(visibilityOf()).toBe("private");
        expect(fetchCalls).toHaveLength(1);
        expect(fetchCalls[0].url).toContain(`/api/agents/${INSTANCE_ID}/sessions/${SID}/visibility`);
        expect(fetchCalls[0].method).toBe("PUT");
        expect(fetchCalls[0].body).toEqual({ visibility: "private" });
    });

    it("rolls the row back and rethrows when the backend refuses", async () => {
        seedAgent([makeSession({ visibility: "public", can_write: true })]);
        fetchStatus = 404;

        await expect(
            useAgentStore.getState().setSessionVisibility(INSTANCE_ID, SID, "private"),
        ).rejects.toThrow();

        expect(visibilityOf()).toBe("public");
    });

    it("treats an absent visibility as public (older Runtime)", () => {
        seedAgent([makeSession({ can_write: true })]);
        // Not "private" → the toggle renders the public (globe) state.
        expect(visibilityOf()).toBeUndefined();
    });
});

describe("closeTab — a viewer must not close the owner's session", () => {
    it("skips POST /close when can_write is false", async () => {
        seedAgent([makeSession({ can_write: false })]);

        await useChatStore.getState().closeTab(INSTANCE_ID, SID);

        expect(fetchCalls.filter((c) => c.url.includes("/close"))).toHaveLength(0);
    });

    it("still notifies the backend for an owned session", async () => {
        seedAgent([makeSession({ can_write: true })]);

        await useChatStore.getState().closeTab(INSTANCE_ID, SID);

        expect(fetchCalls.filter((c) => c.url.includes(`/sessions/${SID}/close`))).toHaveLength(1);
    });
});

describe("openSession — a viewer must not activate the session", () => {
    it("skips POST /open when can_write is false, but still loads history", async () => {
        seedAgent([makeSession({ can_write: false })]);

        await useChatStore.getState().openSession(INSTANCE_ID, SID);

        // No activation: the session stays Closed, so nothing becomes
        // resident in the Runtime and there is nothing for anyone to close.
        expect(fetchCalls.filter((c) => c.url.endsWith(`/sessions/${SID}/open`))).toHaveLength(0);
        // Read-only viewing still works — history is a separate read.
        expect(
            fetchCalls.filter((c) => c.url.includes(`/sessions/${SID}/messages`)),
        ).not.toHaveLength(0);
    });

    it("still activates a session we own", async () => {
        seedAgent([makeSession({ can_write: true })]);

        await useChatStore.getState().openSession(INSTANCE_ID, SID);

        expect(fetchCalls.filter((c) => c.url.endsWith(`/sessions/${SID}/open`))).toHaveLength(1);
    });
});
