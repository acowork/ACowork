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

const invokeMock = vi.fn(async (..._args: unknown[]) => ({}));
vi.mock("@tauri-apps/api/core", () => ({
    invoke: (...args: unknown[]) => (invokeMock as unknown as (...a: unknown[]) => unknown)(...args),
}));

const OLD_URL = "http://192.168.1.10:19876";
const CANDIDATE_URL = "http://192.168.3.10:19876";
const RELAY_URL = "https://c8cb2bed-ffd4-4821-a913-718618482157.relay.acowork.ai";
// Must mirror `UNREACHABLE_HINT_MS` — the shared boot/runtime budget
// in lib/connectivity/gatewayConnectivity.ts.
const CANDIDATE_FALLBACK_MS = 5_000;
const MAX_WAIT_MS = 30_000;

async function flushMicrotasks() {
    for (let i = 0; i < 10; i++) {
        await act(async () => {});
    }
}

/** Extract the URL the gateway config was last pushed with. */
function lastPushedGatewayUrl(): string | undefined {
    const calls = invokeMock.mock.calls.filter((c) => c[0] === "set_gateway_config");
    if (calls.length === 0) return undefined;
    const payload = calls[calls.length - 1][1] as { config?: { url?: string } } | undefined;
    return payload?.config?.url;
}

/** Collect every URL passed to set_gateway_config, in order. */
function allPushedGatewayUrls(): string[] {
    return invokeMock.mock.calls
        .filter((c) => c[0] === "set_gateway_config")
        .map((c) => {
            const payload = c[1] as { config?: { url?: string } } | undefined;
            return payload?.config?.url ?? "";
        });
}

/** The full config (mode + url) of the LAST set_gateway_config push. */
function lastPushedGatewayConfig(): { mode?: string; url?: string } | undefined {
    const calls = invokeMock.mock.calls.filter((c) => c[0] === "set_gateway_config");
    if (calls.length === 0) return undefined;
    const payload = calls[calls.length - 1][1] as
        | { config?: { mode?: string; url?: string } }
        | undefined;
    return payload?.config;
}

describe("SplashScreen 5s candidate chooser", () => {
    beforeEach(() => {
        vi.useFakeTimers();
        vi.stubGlobal("requestAnimationFrame", () => 0);
        invokeMock.mockClear();
        invokeMock.mockImplementation(async (..._args: unknown[]) => ({}));
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

    it("candidate rows use the global accent button style", async () => {
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();

        const row = screen.getByRole("button", { name: new RegExp(CANDIDATE_URL) });
        // Solid accent fill, not an outline row — same global button
        // style the banner pills use.
        expect(row.className).toContain("btn-accent");
        expect(row.className).not.toMatch(/\bborder-/);
    });

    /**
     * Regression: picking a candidate must leave the store at the picked
     * URL, not roll it back to the mount-time (stale) value.
     *
     * Pre-fix bug: `gatewayUrlInput` was frozen at the mount-time store
     * URL (the old, dead address) and only refreshed when the user
     * edited the input. `handleRetry` then compared
     * `gatewayUrlInput (old) !== store (new)`, rewrote the store back to
     * the dead address, and the subsequent `bootGateway` call landed
     * there too — three back-to-back set_gateway_config calls in the
     * order (new, old, old), the reconnect stranded on the dead host.
     */
    it("picking a candidate does NOT roll the store back to the dead URL", async () => {
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();

        // Advance to the 5s fallback so the candidate chooser appears.
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();

        const candidates = useGatewayStore.getState().candidates;
        expect(candidates.length).toBeGreaterThan(0);
        expect(candidates[0].url).toBe(CANDIDATE_URL);

        // Reset the call log so we only inspect what happens from the
        // click onward (the boot path emits its own config push before
        // the user is involved).
        invokeMock.mockClear();

        // Click the candidate. The chooser renders <button> elements
        // keyed by URL; fireEvent.click on the candidate <button> drives
        // onPick synchronously.
        const pickBtn = screen.getByRole("button", { name: new RegExp(CANDIDATE_URL) });
        fireEvent.click(pickBtn);
        await flushMicrotasks();

        // Store must reflect the picked URL, NOT be rolled back to OLD_URL.
        expect(useSettingsStore.getState().gatewayUrl).toBe(CANDIDATE_URL);

        // No `set_gateway_config` push may carry the dead URL after the
        // pick. We tolerate the first push being the picked URL (from
        // `setGatewayUrl` inside `onPick`) plus the `bootGateway` push;
        // any push with the old URL would mean `handleRetry` reverted.
        const pushedUrls = allPushedGatewayUrls();
        expect(pushedUrls.length).toBeGreaterThan(0);
        expect(pushedUrls).not.toContain(OLD_URL);
        // And the final config push lands on the picked host.
        expect(lastPushedGatewayUrl()).toBe(CANDIDATE_URL);
    });

    /**
     * Regression for the relay+http outage: booting in relay mode, the 5s
     * fallback probe surfaced a reachable LAN candidate (http). The old
     * `onPick` persisted the URL but left the mode at `relay` — Rust's
     * `relay_mqtt_wss_url` only accepts `https://` device domains, so
     * every `connect_mqtt` was rejected forever while HTTP health probes
     * still answered (the connection looked "alive" but chat was dead).
     * Picking must now flip the mode to `remote` in the same step, and
     * the FINAL `set_gateway_config` push must be the legal (remote,
     * candidate-url) combo.
     */
    it("picking an http candidate while in relay mode flips the mode to remote", async () => {
        useSettingsStore.setState({
            gatewayMode: "relay",
            gatewayUrl: RELAY_URL,
            gatewayUrlHistory: [RELAY_URL, CANDIDATE_URL],
        });
        render(<SplashScreen onReady={vi.fn()} />);
        await flushMicrotasks();

        // Advance to the 5s fallback so the candidate chooser appears.
        await act(async () => {
            vi.advanceTimersByTime(CANDIDATE_FALLBACK_MS + 100);
        });
        await flushMicrotasks();

        const candidates = useGatewayStore.getState().candidates;
        expect(candidates.length).toBeGreaterThan(0);
        expect(candidates[0].url).toBe(CANDIDATE_URL);

        // Reset the call log so we only inspect what happens from the
        // click onward.
        invokeMock.mockClear();

        const pickBtn = screen.getByRole("button", { name: new RegExp(CANDIDATE_URL) });
        fireEvent.click(pickBtn);
        await flushMicrotasks();

        // Mode flipped to remote in the same step as the URL pick.
        expect(useSettingsStore.getState().gatewayMode).toBe("remote");
        expect(useSettingsStore.getState().gatewayUrl).toBe(CANDIDATE_URL);
        // The final config push is the legal (remote, http-lan) combo —
        // never relay+http, which Rust would reject forever.
        expect(lastPushedGatewayConfig()).toEqual({ mode: "remote", url: CANDIDATE_URL });
    });
});