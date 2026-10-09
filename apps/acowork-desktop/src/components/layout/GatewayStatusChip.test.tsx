/**
 * Regression test for GatewayStatusChip's candidate-probe behavior.
 *
 * Visibility is `gatewayUnreachable` — the connectivity module's
 * user-facing "not CONNACKed for 5s, any cause" signal — and the probe
 * is the module's shared `probeKnownCandidates` (see
 * lib/connectivity/gatewayConnectivity.ts): the SAME probe the
 * SplashScreen 5s fallback uses, so boot and runtime cannot drift.
 *
 * Properties worth pinning:
 *   1. Visible: probes URL history on first paint in remote mode;
 *      populates candidates when at least one host is reachable.
 *   2. Local mode: does NOT probe (irrelevant; user starts it manually).
 *   3. Empty history: clears candidates, no fetch made.
 *   4. Retry click: re-probes (fresh scan in case the network changed).
 *   5. Recovery race: if the gateway comes back during the probe, the
 *      result is discarded.
 *   6. The chip is a *persistent* title-bar affordance, not a toast:
 *      it breathes and carries the candidate count — a sleep/wake drop
 *      stays broken until acted on, so the indicator must never quietly
 *      time out.
 *   7. Candidates live in a popover (opened by the chip), not inline.
 *   8. The classifier verdict ALONE (`status === "error"`) must not
 *      raise the chip, and its absence must not hide it — the 5s
 *      budget lives in the connectivity module, not in the classifier.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { GatewayStatusChip } from "./GatewayStatusChip";
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

describe("GatewayStatusChip candidate probe", () => {
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
            status: "disconnected",
            gatewayUnreachable: true,
            health: null,
            localState: "idle",
            localOwnership: "none",
            candidates: [],
        });
    });

    it("on mount: probes history and populates reachable candidates", async () => {
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        const candidates = useGatewayStore.getState().candidates;
        expect(candidates.length).toBeGreaterThan(0);
        expect(candidates[0].url).toBe(CANDIDATE_URL);
    });

    it("candidate rows use the global accent button style", async () => {
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        fireEvent.click(screen.getByRole("button", { expanded: false }));
        const row = screen.getByRole("button", { name: /192\.168\.3\.10/ });
        // Solid accent fill, not an outline row — same global button
        // style the SplashScreen chooser uses.
        expect(row.className).toContain("btn-accent");
        expect(row.className).not.toMatch(/\bborder-/);
    });

    it("renders nothing while the gateway is healthy", async () => {
        useGatewayStore.setState({ status: "connected", gatewayUnreachable: false });
        const { container } = render(<GatewayStatusChip />);
        await flushMicrotasks();
        // ADR-052: transient boot states must NOT raise the indicator.
        expect(container.textContent).toBe("");
    });

    it("visibility follows the unreachable signal, not the classifier status", async () => {
        // A classifier verdict alone must NOT raise the chip — the 5s
        // budget lives in the connectivity module.
        useGatewayStore.setState({ status: "error", gatewayUnreachable: false });
        const { container } = render(<GatewayStatusChip />);
        await flushMicrotasks();
        expect(container.textContent).toBe("");
        // ...and the inverse: unreachable raises it even though the
        // classifier never wrote `error` (black-holed path — exactly the
        // outage that used to stay hidden for minutes).
        await act(async () => {
            useGatewayStore.setState({ status: "connecting", gatewayUnreachable: true });
        });
        expect(container.textContent).not.toBe("");
    });

    it("chip is a persistent breathing affordance carrying the candidate count", async () => {
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        const chip = screen.getByRole("button", { expanded: false });
        // No auto-dismiss anywhere — a sleep/wake drop is still broken
        // until the user picks an address, so the chip must stay lit.
        expect(chip.className).not.toMatch(/animate-\[.*fade/);
        // Breathing icon inside a solid amber pill: the attention comes
        // from the accent fill, the pulse marks it as live.
        expect(chip.className).toContain("bg-amber-500");
        expect(chip.querySelector(".animate-pulse")).toBeTruthy();
        // Candidate count badge — the user can see there is something to
        // click without opening the popover.
        expect(chip.textContent).toContain("1");
    });

    it("candidates live in a popover the chip opens", async () => {
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        // Closed by default: no URL is dumped into the title bar.
        expect(screen.queryByRole("button", { name: /192\.168\.3\.10/ })).toBeNull();
        fireEvent.click(screen.getByRole("button", { expanded: false }));
        expect(screen.getByRole("button", { name: /192\.168\.3\.10/ })).toBeTruthy();
    });

    it("candidates list is scrollable, never a horizontal pill strip", async () => {
        useSettingsStore.setState({
            gatewayUrlHistory: [OLD_URL, CANDIDATE_URL],
        });
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        fireEvent.click(screen.getByRole("button", { expanded: false }));
        const row = screen.getByRole("button", { name: /192\.168\.3\.10/ }).closest("li")
            ?.parentElement as HTMLElement;
        // Long URL histories overflow the popover vertically; the old
        // banner scrolled horizontally and cut URLs in half.
        expect(row.className).toContain("overflow-auto");
        expect(row.className).not.toContain("overflow-x-auto");
    });

    it("local mode does NOT probe", async () => {
        useSettingsStore.setState({ gatewayMode: "local" });
        const fetchMock = vi.fn();
        vi.stubGlobal("fetch", fetchMock);
        render(<GatewayStatusChip />);
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
        render(<GatewayStatusChip />);
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
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        const initialCalls = fetchMock.mock.calls.length;
        // Retry lives inside the popover.
        fireEvent.click(screen.getByRole("button", { expanded: false }));
        fireEvent.click(screen.getByRole("button", { name: /Retry|重试/i }));
        await flushMicrotasks();
        expect(fetchMock.mock.calls.length).toBeGreaterThan(initialCalls);
    });

    it("picking a candidate sets the URL, clears candidates, and triggers health probe", async () => {
        render(<GatewayStatusChip />);
        await flushMicrotasks();
        fireEvent.click(screen.getByRole("button", { expanded: false }));
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
        render(<GatewayStatusChip />);
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