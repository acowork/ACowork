/**
 * ADR-085 regression — the FAILED lifecycle detail must survive the
 * transient stamp.
 *
 * Symptom the latch removes: a Runtime whose Phase B fails publishes
 * `state=FAILED, detail=<reason>` and exits immediately; the LWT
 * OFFLINE lands milliseconds later, forcing `meta.lifecycle` back to
 * "offline" and wiping `meta.lifecycle_detail` (the Gateway also drops
 * its running_agents entry, so REST cannot recover the reason either).
 * `waitForAgentReady` polls every 500ms and in the common case only
 * ever observes the post-LWT state — without the latch the user gets a
 * generic "Agent is no longer alive before becoming ready" instead of
 * the actual startup failure (ADR-085 goal 4: failures are visible).
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

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
import type { AgentInfo } from "../lib/types";

const AGENT_ID = "com.acowork.architect";
const INSTANCE_ID = "b7f0c6c2-4f10-4f5e-9a1e-3d8e9f2a1c33";
const FAILURE = "session init: sqlite open failed";

function makeMeta(overrides: Partial<AgentInfo> = {}): AgentInfo {
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
        lifecycle: "starting",
        debug_state: "disabled",
        debug_port: null,
        workspace: "",
        workspace_config_json: null,
        current_embed_dim: null,
        migration: null,
        started_at: "2026-01-01T00:00:00Z",
        last_interaction_at: "2026-01-01T00:00:00Z",
        ...overrides,
    } as AgentInfo;
}

function seedAgent(meta: AgentInfo) {
    useAgentStore.setState({
        agents: {
            [INSTANCE_ID]: {
                meta,
                profile: {} as never,
                sessions: [],
                sessionTitle: undefined,
                pagination: { currentPage: 1, totalPages: 1, totalCount: 0, pageSize: 20 },
                isLoading: false,
            },
        },
        selectedAgentId: INSTANCE_ID,
        lastStartupFailure: {},
        error: null,
        // waitForAgentReady polls fetchAgents (REST); the latch test must
        // observe the store as-is, so the poll is a no-op.
        fetchAgents: async () => {},
    } as never);
}

beforeEach(() => {
    useAgentStore.setState({ agents: {}, selectedAgentId: null, lastStartupFailure: {} } as never);
});

describe("updateAgentLiveness — FAILED detail latch (ADR-085)", () => {
    it("latches the failure reason and keeps it across the LWT OFFLINE wipe", () => {
        seedAgent(makeMeta());
        const s = () => useAgentStore.getState();

        // The transient FAILED stamp (online=true, state=failed, detail).
        s().updateAgentLiveness(INSTANCE_ID, true, "failed", FAILURE);
        expect(s().agents[INSTANCE_ID]!.meta.lifecycle).toBe("failed");
        expect(s().lastStartupFailure[INSTANCE_ID]).toBe(FAILURE);

        // Milliseconds later the LWT OFFLINE wipes meta — the latch survives.
        s().updateAgentLiveness(INSTANCE_ID, false, "offline", "");
        expect(s().agents[INSTANCE_ID]!.meta.alive).toBe(false);
        expect(s().agents[INSTANCE_ID]!.meta.lifecycle).toBe("offline");
        expect(s().agents[INSTANCE_ID]!.meta.lifecycle_detail).toBe("");
        expect(s().lastStartupFailure[INSTANCE_ID]).toBe(FAILURE);
    });

    it("waitForAgentReady surfaces the latched reason after the wipe", async () => {
        seedAgent(makeMeta());
        const s = () => useAgentStore.getState();

        s().updateAgentLiveness(INSTANCE_ID, true, "failed", FAILURE);
        s().updateAgentLiveness(INSTANCE_ID, false, "offline", "");

        await expect(s().waitForAgentReady(INSTANCE_ID)).rejects.toThrow(
            `Agent failed to start: ${FAILURE}`,
        );
    });

    it("waitForAgentReady keeps the generic error when nothing was latched", async () => {
        seedAgent(makeMeta({ alive: false, lifecycle: "offline" }));

        await expect(
            useAgentStore.getState().waitForAgentReady(INSTANCE_ID),
        ).rejects.toThrow("Agent is no longer alive before becoming ready");
    });

    it("a successful start (sessions_ready) clears the stale latch", () => {
        seedAgent(makeMeta());
        const s = () => useAgentStore.getState();

        s().updateAgentLiveness(INSTANCE_ID, true, "failed", FAILURE);
        expect(s().lastStartupFailure[INSTANCE_ID]).toBe(FAILURE);

        s().updateAgentLiveness(INSTANCE_ID, true, "sessions_ready", "");
        expect(INSTANCE_ID in s().lastStartupFailure).toBe(false);
    });

    it("a fresh startAgent attempt invalidates the previous failure", async () => {
        seedAgent(makeMeta({ alive: false, lifecycle: "offline" }));
        const s = () => useAgentStore.getState();

        s().updateAgentLiveness(INSTANCE_ID, true, "failed", FAILURE);
        expect(s().lastStartupFailure[INSTANCE_ID]).toBe(FAILURE);

        // invoke is mocked to reject — startAgent fails, but the latch is
        // cleared BEFORE the attempt so a stale reason can never be
        // presented as this attempt's failure.
        await expect(s().startAgent(INSTANCE_ID)).rejects.toThrow();
        expect(INSTANCE_ID in useAgentStore.getState().lastStartupFailure).toBe(false);
    });
});
