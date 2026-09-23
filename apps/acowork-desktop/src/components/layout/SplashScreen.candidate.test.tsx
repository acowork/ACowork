/**
 * Self-check for the SplashScreen 5s-fallback candidate chooser.
 *
 * Scenario: remote Gateway at URL `OLD_URL` is unreachable, but the
 * user's URL history contains `CANDIDATE_URL` which IS reachable. After
 * the 5s fallback window elapses, SplashScreen must:
 *   1. Probe the history concurrently.
 *   3. Render the candidate chooser INSIDE the loading view (NOT yet the
 *      30s timeout view) so the user can pick one before the hard
 *      timeout fires.
 *   2. Picking a candidate persists the URL and re-runs the boot flow.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { SplashScreen } from "./SplashScreen";
import { useSettingsStore } from "../../stores/settingsStore";
import { useGatewayStore } from "../../stores/gatewayStore";

vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => ({})),
}));

const OLD_URL = "http://192.168.1.10:19876";
const CANDIDATE_URL = "http://192.168.3.10:19876";
// Must match CANDIDATE_FALLBACK_MS in SplashScreen.tsx
const CANDIDATE_FALLBACK_MS = 5_000;
const MAX_WAIT_MS = 30_000;

async function flushMicrotasks() {
    for (let i = 0; i < 10; i++) {
        await act(async () => {});
    }
}

describe("SplashScreen 5s candidate chooser", () => {
    beforeEach(() => {
        vi.useFakeTimers();
        vi.stubGlobal("requestAnimationFrame", () => 0);
        // `OLD_URL` (192.168.1.x) is unreachable. `CANDIDATE_URL`
        // (192.168.3.x) is reachable. Probe URLs are `${url}/health` so
        // match on host substring, not "candidate".
        vi.stubGlobal(
            "fetch",
            vi.fn((url: string) => {
                if (String(url).includes("192.168.3")) {
                    return Promise.resolve({
                        ok: true,
                        json: async () => ({ status: "ok", version: "0" }),
                    } as Response);
                }
                return Promise.reject(new Error("unreachable"));
            }),
        );
        useSettingsStore.setState({
            gatewayMode: "remote",
            gatewayUrl: OLD_URL,
            gatewayUrlHistory: [OLD_URL, CANDIDATE_URL],
        });
        useGatewayStore.setState({
            status: "disconnected",
            health: null,
            localState: "idle",
            localOwnership: "none",
            candidates: [],
        });
    });

    afterEach(() => {
        vi.useRealTimers();
        vi.unstubAllGlobals();
        vi.restoreAllMocks();
    });

    it("surfaces a candidate chooser inside the loading view after the 5s fallback", async () => {
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();

        // Before the 5s window: no chooser yet.
        expect(screen.queryByText(/reachable/i)).toBeNull();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);

        // Advance just past the fallback window (still well below the
        // 30s hard timeout so the timeout view doesn't appear).
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();

        // Candidates populated in the store.
        const candidates = useGatewayStore.getState().candidates;
        expect(candidates.length).toBeGreaterThan(0);
        expect(candidates[0].url).toBe(CANDIDATE_URL);

        // Chooser is rendered with the candidate URL visible.
        expect(screen.getByText(CANDIDATE_URL)).toBeTruthy();
        // Hard timeout view has NOT appeared yet.
        expect(screen.queryByText("Retry Connection")).toBeNull();
    });

    it("does NOT fire the fallback in local mode (history is irrelevant)", async () => {
        useSettingsStore.setState({ gatewayMode: "local" });
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
        expect(screen.queryByText(CANDIDATE_URL)).toBeNull();
    });

    it("does not surface a candidate when ALL probes fail", async () => {
        // Override: everything fails.
        vi.stubGlobal(
            "fetch",
            vi.fn(() => Promise.reject(new Error("down"))),
        );
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
        expect(screen.queryByText(CANDIDATE_URL)).toBeNull();
    });

    it("happy path: connected before fallback fires → candidates stay empty", async () => {
        // Make everything (incl. OLD_URL) reachable.
        vi.stubGlobal(
            "fetch",
            vi.fn(async () => ({
                ok: true,
                json: async () => ({ status: "ok", version: "0" }),
            } as Response)),
        );
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();
        // Health poll succeeds → status flips to connected.
        await act(async () => {
            vi.advanceTimersByTime(2_000);
        });
        await flushMicrotasks();
        expect(useGatewayStore.getState().status).toBe("connected");
        // Even after the fallback window, no chooser appears because the
        // chooser is gated on `candidates.length > 0`.
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS);
        });
        await flushMicrotasks();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
        expect(screen.queryByText(CANDIDATE_URL)).toBeNull();
    });
});