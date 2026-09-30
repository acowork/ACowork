/**
 * Regression coverage for the MQTT `agent_status` HTTP double-check.
 *
 * 2026-09-02 09:12 incident: during system sleep the MQTT connection
 * dropped (KeepAlive timeout) → Gateway marked the agent offline → the
 * desktop rendered it as offline even though the Runtime process itself
 * stayed alive. The fix lives in `handleMessageEvent`'s `agent_status`
 * branch (chatStore.ts) which, when `online=false` arrives, fires an
 * HTTP probe of the Runtime's `/health` endpoint via the Gateway
 * reverse-proxy (`/api/agents/{id}/health`). A 2xx answer overrides the
 * MQTT signal back to online so the desktop does NOT mis-render offline.
 *
 * These tests pin:
 *   - online=true → no HTTP probe (avoid wasted work on every status tick)
 *   - online=false + health=alive → override back to online=true
 *   - online=false + health=dead  → stays offline (genuine shutdown)
 *   - online=false + health throws → stays offline (defensive)
 *
 * Auto-sleep was retired in Sept 2026 — `sleeping` is no longer part
 * of the wire, so the call shape is `(agentId, online)`.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// ── Hoisted mocks: vi.mock factories are hoisted to the top of the file,
//    so any shared state they reference must also be hoisted via
//    `vi.hoisted`. This is the standard vitest pattern for sharing spies
//    between the mock factory and the test body.

const { updateCalls, mockVerifyAgentHealth, mockUpdateAgentLiveness } =
    vi.hoisted(() => {
        const updateCalls: Array<{ agentId: string; alive: boolean }> = [];
        const mockVerifyAgentHealth = vi.fn<
            [agentId: string, timeoutMs?: number, gatewayUrl?: string],
            Promise<boolean>
        >();
        const mockUpdateAgentLiveness = vi.fn(
            (agentId: string, online: boolean) => {
                updateCalls.push({ agentId, alive: online });
            },
        );
        return {
            updateCalls,
            mockVerifyAgentHealth,
            mockUpdateAgentLiveness,
        };
    });

// ── Mock the HTTP health check before chatStore imports it ───────────────

vi.mock("../lib/gateway-api", async () => {
    const actual =
        await vi.importActual<typeof import("../lib/gateway-api")>(
            "../lib/gateway-api",
        );
    return {
        ...actual,
        verifyAgentHealth: mockVerifyAgentHealth,
    };
});

// ── Mock the agent store so we can spy on updateAgentLiveness ───────

vi.mock("./agentStore", () => ({
    useAgentStore: {
        getState: () => ({
            updateAgentLiveness: mockUpdateAgentLiveness,
        }),
    },
}));

// ── Now import the SUT ──────────────────────────────────────────────────

import { handleMessageEvent, useChatStore } from "./chatStore";

const AGENT = "com.acowork.architect";

beforeEach(() => {
    updateCalls.length = 0;
    // `mockReset` clears implementations; re-establish a safe default so
    // tests that don't care about the probe's answer still get a resolved
    // promise (instead of `undefined`, which would crash `.then()`).
    mockVerifyAgentHealth.mockReset();
    mockVerifyAgentHealth.mockResolvedValue(false);
    mockUpdateAgentLiveness.mockClear();
});

afterEach(() => {
    vi.useRealTimers();
});

// ── Tests ────────────────────────────────────────────────────────────────

describe("agent_status handler: HTTP double-check on offline events", () => {
    it("does NOT probe /health when online=true arrives (avoid wasted work)", async () => {
        // Drive the handler with online=true — the most common case. The
        // MQTT signal is authoritative when it says alive; we don't burn
        // an HTTP round-trip per status tick.
        handleMessageEvent(
            { type: "agent_status", instance_id: AGENT, online: true },
            useChatStore.setState,
            useChatStore.getState,
            AGENT,
        );

        // Let any microtasks settle.
        await Promise.resolve();
        await Promise.resolve();

        expect(mockVerifyAgentHealth).not.toHaveBeenCalled();
        // updateAgentLiveness must still be called once (with online=true).
        expect(mockUpdateAgentLiveness).toHaveBeenCalledTimes(1);
        expect(mockUpdateAgentLiveness).toHaveBeenCalledWith(AGENT, true, undefined, undefined);
    });

    it("probes /health on online=false AND overrides back to online when the Runtime is alive", async () => {
        // Simulates the 09:12 incident: MQTT drops (system sleep) →
        // Gateway republishes offline → WITHOUT the fix the desktop
        // would render offline. WITH the fix, the probe finds the
        // Runtime alive and the state is corrected back to online.
        mockVerifyAgentHealth.mockResolvedValue(true);

        handleMessageEvent(
            { type: "agent_status", instance_id: AGENT, online: false },
            useChatStore.setState,
            useChatStore.getState,
            AGENT,
        );

        // First call: the agent_status event itself.
        expect(mockUpdateAgentLiveness).toHaveBeenCalledTimes(1);
        expect(mockUpdateAgentLiveness).toHaveBeenLastCalledWith(AGENT, false, undefined, undefined);

        // Let the probe's promise resolve.
        await vi.waitFor(() => {
            expect(mockVerifyAgentHealth).toHaveBeenCalledWith(AGENT);
        });
        await Promise.resolve();

        // Second call: the override after the probe resolves.
        expect(mockUpdateAgentLiveness).toHaveBeenCalledTimes(2);
        expect(mockUpdateAgentLiveness).toHaveBeenLastCalledWith(AGENT, true);
    });

    it("stays offline when the probe finds the Runtime dead (genuine shutdown)", async () => {
        // Even with MQTT saying offline, we confirm with HTTP. If HTTP
        // also says dead, we leave the agent offline — no second
        // updateAgentLiveness call.
        mockVerifyAgentHealth.mockResolvedValue(false);

        handleMessageEvent(
            { type: "agent_status", instance_id: AGENT, online: false },
            useChatStore.setState,
            useChatStore.getState,
            AGENT,
        );

        await vi.waitFor(() => {
            expect(mockVerifyAgentHealth).toHaveBeenCalledWith(AGENT);
        });
        await Promise.resolve();

        // Only the initial offline update — no override back to online.
        expect(mockUpdateAgentLiveness).toHaveBeenCalledTimes(1);
        expect(mockUpdateAgentLiveness).toHaveBeenCalledWith(AGENT, false, undefined, undefined);
    });

    it("does not crash if the probe throws (network error, DNS, etc.)", async () => {
        // Defensive: verifyAgentHealth's own try/catch should swallow
        // exceptions and return false. We verify that even if a
        // throw leaks, the desktop doesn't blow up — the .then handler
        // must not reject uncaught.
        mockVerifyAgentHealth.mockRejectedValue(new Error("ECONNREFUSED"));

        expect(() => {
            handleMessageEvent(
                { type: "agent_status", instance_id: AGENT, online: false },
                useChatStore.setState,
                useChatStore.getState,
                AGENT,
            );
        }).not.toThrow();

        // Drain the microtask queue so the .then handler runs.
        await new Promise((r) => setTimeout(r, 10));

        // No override — only the initial offline update.
        expect(mockUpdateAgentLiveness).toHaveBeenCalledTimes(1);
        expect(mockUpdateAgentLiveness).toHaveBeenCalledWith(AGENT, false, undefined, undefined);
    });

    it("ignores malformed events without an instance_id", () => {
        handleMessageEvent(
            { type: "agent_status", online: false },
            useChatStore.setState,
            useChatStore.getState,
            AGENT,
        );

        // No instance_id → no update, no probe.
        expect(mockUpdateAgentLiveness).not.toHaveBeenCalled();
        expect(mockVerifyAgentHealth).not.toHaveBeenCalled();
    });

    it("ignores events without a defined online flag", () => {
        handleMessageEvent(
            { type: "agent_status", instance_id: AGENT },
            useChatStore.setState,
            useChatStore.getState,
            AGENT,
        );

        // No `online` → no update, no probe.
        expect(mockUpdateAgentLiveness).not.toHaveBeenCalled();
        expect(mockVerifyAgentHealth).not.toHaveBeenCalled();
    });
});
