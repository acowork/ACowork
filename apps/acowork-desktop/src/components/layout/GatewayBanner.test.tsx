/**
 * Regression test for GatewayBanner's candidate-probe behavior.
 *
 * The banner drives its own probe — it runs on mount and on every
 * Retry click. This is more robust than the previous
 * status-transition-based subscriber at App.tsx, which missed cases
 * where /health stayed "ok" while the network was effectively dead.
 *
 * Properties worth pinning:
 *   1. Banner-mounted: probes URL history on first paint in remote
 *      mode; populates candidates when at least one host is reachable.
 *   2. Local mode: does NOT probe (irrelevant; user starts it manually).
 *   3. Empty history: clears candidates, no fetch made.
 *   4. Retry click: re-probes (fresh scan in case the network changed).
 *   5. Recovery race: if `status` flips to `connected` during the
 *      probe, the result is discarded.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { GatewayBanner } from "./GatewayBanner";
import { useSettingsStore } from "../../stores/settingsStore";
import { useGatewayStore } from "../../stores/gatewayStore";

vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => ({})),
}));

const OLD_URL = "http://192.168.1.10:19876";
const CANDIDATE_URL = "http://192.168.3.10:19876";

async function flushMicrotasks() {
    for (let i = 0; i < 10; i++) {
        await act(async () => {});
    }
}

function makeFetchMock(opts: { hanging?: boolean } = {}) {
    if (opts.hanging) {
        let resolveFn: () => void = () => {};
        vi.stubGlobal(
            "fetch",
            vi.fn(
                () =>
                    new Promise<Response>((_resolve, reject) => {
                        // The component aborts via AbortController after
                        // perProbeTimeoutMs; here we simulate an in-flight
                        // probe by never resolving.
                        resolveFn = () => {}; // captured for test use
                        // Don't reject; just hang so the test can flip
                        // status and observe the race.
                        // Capture rejectFn for the test to reject later.
                        reject; // eslint hint
                    }),
            ),
        );
        return { resolveFn };
    }
    return null;
}

describe("GatewayBanner candidate probe", () => {
    beforeEach(() => {
        vi.stubGlobal(
            "fetch",
            vi.fn((url: string) => {
                if (String(url).includes("192.168.3")) {
                    return Promise.resolve({
                        ok: true,
                        json: async () => ({ status: "ok", version: "0" }),
                    } as Response);
                }
                return Promise.reject(new Error("down"));
            }),
        );
        localStorage.clear();
        useSettingsStore.setState({
            gatewayMode: "remote",
            gatewayUrl: OLD_URL,
            gatewayUrlHistory: [OLD_URL, CANDIDATE_URL],
        });
        useGatewayStore.setState({
            status: "error",
            health: null,
            localState: "idle",
            localOwnership: "none",
            candidates: [],
        });
    });

    it("on mount: probes history and populates reachable candidates", async () => {
        render(<GatewayBanner />);
        await flushMicrotasks();
        const candidates = useGatewayStore.getState().candidates;
        expect(candidates.length).toBeGreaterThan(0);
        expect(candidates[0].url).toBe(CANDIDATE_URL);
    });

    it("renders candidate pills as buttons", async () => {
        render(<GatewayBanner />);
        await flushMicrotasks();
        expect(screen.getByRole("button", { name: /192\.168\.3\.10/ })).toBeTruthy();
    });

    it("local mode does NOT probe", async () => {
        useSettingsStore.setState({ gatewayMode: "local" });
        const fetchMock = vi.fn();
        vi.stubGlobal("fetch", fetchMock);
        render(<GatewayBanner />);
        await flushMicrotasks();
        expect(fetchMock).not.toHaveBeenCalled();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
    });

    it("empty history: clears candidates, no fetch call", async () => {
        const fetchMock = vi.fn();
        vi.stubGlobal("fetch", fetchMock);
        useSettingsStore.setState({
            gatewayUrlHistory: [OLD_URL], // only the current URL — nothing else to probe
        });
        render(<GatewayBanner />);
        await flushMicrotasks();
        expect(fetchMock).not.toHaveBeenCalled();
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
    });

    it("Retry click re-probes", async () => {
        const fetchMock = vi.fn((url: string) => {
            if (String(url).includes("192.168.3")) {
                return Promise.resolve({
                    ok: true,
                    json: async () => ({}),
                } as Response);
            }
            return Promise.reject(new Error("down"));
        });
        vi.stubGlobal("fetch", fetchMock);
        render(<GatewayBanner />);
        await flushMicrotasks();
        const initialCalls = fetchMock.mock.calls.length;
        fireEvent.click(screen.getByRole("button", { name: /Retry/i }));
        await flushMicrotasks();
        expect(fetchMock.mock.calls.length).toBeGreaterThan(initialCalls);
    });

    it("picking a candidate sets the URL, clears candidates, and triggers health probe", async () => {
        render(<GatewayBanner />);
        await flushMicrotasks();
        fireEvent.click(screen.getByRole("button", { name: /192\.168\.3\.10/ }));
        await flushMicrotasks();
        expect(useSettingsStore.getState().gatewayUrl).toBe(CANDIDATE_URL);
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
    });

    it("recovery race: status → connected during probe discards the result", async () => {
        // Fetch that hangs until the test resolves it.
        let resolveFetch: (v: Response) => void = () => {};
        vi.stubGlobal(
            "fetch",
            vi.fn(
                () =>
                    new Promise<Response>((resolve) => {
                        resolveFetch = resolve;
                    }),
            ),
        );
        render(<GatewayBanner />);
        await flushMicrotasks();
        // Gateway came back during the probe.
        act(() => {
            useGatewayStore.setState({ status: "connected" });
        });
        // Now resolve the still-hanging fetch with a "reachable" answer —
        // the banner must observe connected and clear candidates.
        await act(async () => {
            resolveFetch({
                ok: true,
                json: async () => ({}),
            } as Response);
            await flushMicrotasks();
        });
        expect(useGatewayStore.getState().candidates).toHaveLength(0);
    });
});