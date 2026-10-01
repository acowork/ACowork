/**
 * ADR-076 §决策 4 — the single read-only gate for the composer toolbar.
 *
 * Regression guard for the bug this module was extracted to prevent: the
 * session's write controls were gated by threading a `readOnly` prop into
 * each child, three children were missed, and a *disabled* button rendered
 * with no disabled styling was indistinguishable from an enabled one. So
 * two things are asserted here:
 *
 *   1. `can_write === false` is the ONLY input that produces read-only, and
 *      an unknown answer (row missing, field absent from an older Runtime)
 *      degrades to writable so an optimistically-created session and a
 *      version-skewed Runtime don't lock the toolbar.
 *
 *   2. The shared `toolbarButton` token actually carries `disabled:` styles.
 *      This is the assertion that would have failed before the fix — a
 *      disabled attribute with no visual difference is the reported symptom.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook } from "@testing-library/react";

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

import { useAgentStore } from "../stores/agentStore";
import { useChatStore } from "../stores/chatStore";
import {
    isReadOnlySession,
    useActiveSessionReadOnly,
    useReadOnlySessionIds,
} from "./session-write-access";
import { toolbarButton } from "./ui-styles";
import type { AgentInfo } from "../stores/agentStore";
import type { SessionInfo } from "./types";

const AGENT_ID = "com.acowork.architect";
const INSTANCE_ID = "b7f0c6c2-4f10-4f5e-9a1e-3d8e9f2a1c33";
const SID = "sess-shared";

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
        ready: true,
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
            },
        },
        selectedAgentId: INSTANCE_ID,
    });
}

beforeEach(() => {
    useAgentStore.setState({ agents: {}, selectedAgentId: null, loading: false, error: null });
    useChatStore.setState({ agentStates: {} });
});

afterEach(() => {
    vi.unstubAllGlobals();
});

describe("isReadOnlySession — the raw predicate", () => {
    it("locks only on an explicit can_write: false", () => {
        expect(isReadOnlySession(false)).toBe(true);
        expect(isReadOnlySession(true)).toBe(false);
        // Older Runtime omits the field; a brand-new session is not in the
        // list yet. Both must stay writable.
        expect(isReadOnlySession(undefined)).toBe(false);
    });

    it("does not infer read-only from visibility", () => {
        // A public session is readable by everyone but owned by one account;
        // the backend's can_write is the only authority.
        const s = makeSession({ visibility: "public" });
        expect(isReadOnlySession(s.can_write)).toBe(false);
    });
});

describe("useActiveSessionReadOnly", () => {
    it("is true for a session shared with us", () => {
        seedAgent([makeSession({ visibility: "public", can_write: false })]);
        useChatStore.setState({
            agentStates: { [INSTANCE_ID]: { activeSessionId: SID } as never },
        });
        const { result } = renderHook(() => useActiveSessionReadOnly());
        expect(result.current).toBe(true);
    });

    it("is false for a session we own, and for unknown sessions", () => {
        seedAgent([makeSession({ can_write: true })]);
        useChatStore.setState({
            agentStates: { [INSTANCE_ID]: { activeSessionId: SID } as never },
        });
        expect(renderHook(() => useActiveSessionReadOnly()).result.current).toBe(false);

        // Optimistically created: the active session is not in the list yet.
        useChatStore.setState({
            agentStates: { [INSTANCE_ID]: { activeSessionId: "sess-new" } as never },
        });
        expect(renderHook(() => useActiveSessionReadOnly()).result.current).toBe(false);

        // No agent / no session selected at all.
        useAgentStore.setState({ selectedAgentId: null });
        expect(renderHook(() => useActiveSessionReadOnly()).result.current).toBe(false);
    });
});

describe("useReadOnlySessionIds", () => {
    it("collects only the shared sessions", () => {
        seedAgent([
            makeSession({ session_id: "a", can_write: true }),
            makeSession({ session_id: "b", can_write: false }),
            makeSession({ session_id: "c" }),
        ]);
        const { result } = renderHook(() => useReadOnlySessionIds(INSTANCE_ID));
        expect([...result.current]).toEqual(["b"]);
    });

    it("returns an empty set when there is no agent", () => {
        expect([...renderHook(() => useReadOnlySessionIds(null)).result.current]).toEqual([]);
    });
});

describe("toolbarButton — disabled must be visible", () => {
    it("carries disabled styling so a locked control cannot look clickable", () => {
        // The reported bug: the visibility toggle had `disabled` set but was
        // styled with the bare toolbar token, so it looked enabled and hover
        // still lit up.
        expect(toolbarButton).toMatch(/disabled:opacity-50/);
        expect(toolbarButton).toMatch(/disabled:cursor-not-allowed/);
        expect(toolbarButton).toMatch(/disabled:hover:bg-transparent/);
    });
});
