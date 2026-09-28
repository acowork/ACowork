/**
 * Self-check for the in-flight dedup gate around `startAgent`.
 *
 * Regression: ChatPanel's big Play button used to call
 * `startAgentAndSyncUI` directly with no dedup state. Within the
 * 1-3s window between `invoke("start_agent")` and the MQTT
 * `agent_status online` event, `selectedAgent.alive` was still
 * false, so the Play button kept rendering and the user could
 * mash-click it. The second invoke reached the Gateway's
 * `running_agents` guard and surfaced a misleading
 * "Agent X is already running" toast.
 *
 * Fix: `tryStartAgent` puts the agent id into
 * `startingAgentIds` for the whole round-trip; a second call
 * inside that window returns false instead of touching the
 * backend. The flag must clear on both success and failure paths
 * (finally block), and a failed start must reject so the caller
 * can surface the real error.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// ── Mock Tauri invoke: tryStartAgent drives start_agent through this ──

const mockListAgents = vi.fn<[], Promise<unknown[]>>();
const mockStartAgent = vi.fn<[], Promise<unknown>>();

vi.mock("@tauri-apps/api/core", () => ({
    invoke: (cmd: string) => {
        if (cmd === "list_agents") return mockListAgents();
        if (cmd === "start_agent") return mockStartAgent();
        return Promise.reject(new Error(`Unexpected invoke: ${cmd}`));
    },
}));

vi.mock("../lib/logger", () => ({
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {}, },
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
import type { AgentInfo } from "./agentStore";

const AGENT_ID = "com.acowork.architect";
const INSTANCE_ID = "b7f0c6c2-4f10-4f5e-9a1e-3d8e9f2a1c33";

function makeMeta(overrides: Partial<AgentInfo>): AgentInfo {
    return {
        agent_id: AGENT_ID,
        instance_id: INSTANCE_ID,
        name: "Architect",
        version: "1.0.0",
        avatar: null,
        builtin_avatar: null,
        display_name: null,
        role: null,
        alive: false,
        ready: false,
        debug_state: "disabled",
        debug_port: null,
        workspace: "",
        workspace_config_json: null,
        current_embed_dim: null,
        migration: null,
        started_at: "2026-01-01T00:00:00Z",
        last_interaction_at: "2026-01-01T00:00:00Z",
        ...overrides,
    };
}

function seedAgent(meta: Partial<AgentInfo>) {
    useAgentStore.setState({
        agents: {
            [INSTANCE_ID]: {
                meta: makeMeta(meta),
                profile: {} as never,
                sessions: [],
                sessionTitle: undefined,
                pagination: { currentPage: 1, totalPages: 1, totalCount: 0, pageSize: 20 },
                isLoading: false,
            },
        },
        selectedAgentId: INSTANCE_ID,
    });
}

/** Block the startAgent waiter until we choose to release it. */
function blockStartAgent(): { release: () => void } {
    let release!: () => void;
    mockStartAgent.mockImplementationOnce(() => new Promise((resolve) => {
        // The waiter is registered inside the invoke microtask; we
        // hold the resolve so the test can deterministically click
        // a second time while the first is still in flight.
        release = () => resolve({});
    }));
    return { release: () => release() };
}

beforeEach(() => {
    useAgentStore.setState({
        agents: {},
        selectedAgentId: null,
        loading: false,
        error: null,
        startingAgentIds: new Set<string>(),
    });
    mockListAgents.mockReset();
    mockStartAgent.mockReset();
});

afterEach(() => {
    vi.useRealTimers();
});

describe("agentStore.tryStartAgent — dedup gate", () => {
    it("marks the agent as starting while the round-trip is in flight", async () => {
        seedAgent({ alive: false, ready: false });
        const blocker = blockStartAgent();

        const p = useAgentStore.getState().tryStartAgent(INSTANCE_ID);
        // Wait one microtask so the invoke + waiter registration settles.
        await new Promise((r) => setTimeout(r, 0));
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(true);

        // Release the in-flight startAgent by faking the MQTT online event.
        blocker.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);
        await expect(p).resolves.toBe(true);
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(false);
    });

    it("drops a second click while the first is still in flight (returns false)", async () => {
        seedAgent({ alive: false, ready: false });
        const blocker = blockStartAgent();

        const first = useAgentStore.getState().tryStartAgent(INSTANCE_ID);
        await new Promise((r) => setTimeout(r, 0));

        // The button mash: a second click inside the MQTT-online window.
        const second = useAgentStore.getState().tryStartAgent(INSTANCE_ID);
        await expect(second).resolves.toBe(false);

        // Only the first round-trip should have reached the backend.
        expect(mockStartAgent).toHaveBeenCalledTimes(1);

        // Cleanup so afterEach doesn't leak a pending waiter.
        blocker.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);
        await first;
    });

    it("clears startingAgentIds on failure so the user can retry", async () => {
        seedAgent({ alive: false, ready: false });
        // Synchronous throw before any waiter is registered — the
        // finally block still has to clear the dedup flag.
        mockStartAgent.mockImplementationOnce(() => {
            throw new Error("boom");
        });

        await expect(
            useAgentStore.getState().tryStartAgent(INSTANCE_ID),
        ).rejects.toThrow(/boom/);
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(false);

        // After the failure the dedup gate must let the next click through.
        const blocker = blockStartAgent();
        const p = useAgentStore.getState().tryStartAgent(INSTANCE_ID);
        await new Promise((r) => setTimeout(r, 0));
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(true);

        blocker.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);
        await expect(p).resolves.toBe(true);
    });

    it("runs the optional `run` hook inside the in-flight window", async () => {
        seedAgent({ alive: false, ready: false });
        const blocker = blockStartAgent();
        const run = vi.fn().mockResolvedValue(undefined);

        const p = useAgentStore.getState().tryStartAgent(INSTANCE_ID, { run });
        await new Promise((r) => setTimeout(r, 0));

        // `run` must execute AFTER startAgent resolves (after the
        // MQTT online event), not before — otherwise session-init
        // fetches race the runtime's HTTP listener.
        expect(run).not.toHaveBeenCalled();

        blocker.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);
        await expect(p).resolves.toBe(true);
        expect(run).toHaveBeenCalledWith(INSTANCE_ID);
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(false);
    });

    it("clears startingAgentIds even when the `run` hook throws", async () => {
        seedAgent({ alive: false, ready: false });
        const blocker = blockStartAgent();
        const run = vi.fn().mockRejectedValue(new Error("session init failed"));

        const p = useAgentStore.getState().tryStartAgent(INSTANCE_ID, { run });
        blocker.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);

        await expect(p).rejects.toThrow(/session init failed/);
        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(false);
    });

    it("supports concurrent starts for DIFFERENT agents independently", async () => {
        const OTHER_INSTANCE = "9f1e2d3c-8a7b-4c5d-9e0f-1a2b3c4d5e6f";
        seedAgent({ alive: false, ready: false });
        useAgentStore.setState((state) => ({
            agents: {
                ...state.agents,
                [OTHER_INSTANCE]: {
                    meta: makeMeta({ agent_id: "com.acowork.other", instance_id: OTHER_INSTANCE }),
                    profile: {} as never,
                    sessions: [],
                    sessionTitle: undefined,
                    pagination: { currentPage: 1, totalPages: 1, totalCount: 0, pageSize: 20 },
                    isLoading: false,
                },
            },
        }));

        const blockerA = blockStartAgent();
        const blockerB = blockStartAgent();
        const pA = useAgentStore.getState().tryStartAgent(INSTANCE_ID);
        const pB = useAgentStore.getState().tryStartAgent(OTHER_INSTANCE);
        await new Promise((r) => setTimeout(r, 0));

        expect(useAgentStore.getState().startingAgentIds.has(INSTANCE_ID)).toBe(true);
        expect(useAgentStore.getState().startingAgentIds.has(OTHER_INSTANCE)).toBe(true);

        blockerA.release();
        useAgentStore.getState().updateAgentLiveness(INSTANCE_ID, true);
        blockerB.release();
        useAgentStore.getState().updateAgentLiveness(OTHER_INSTANCE, true);
        await Promise.all([pA, pB]);

        expect(mockStartAgent).toHaveBeenCalledTimes(2);
    });
});