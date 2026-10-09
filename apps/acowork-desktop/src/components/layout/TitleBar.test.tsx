/**
 * TitleBar view-toggle buttons.
 *
 * Why this file exists:
 *   The two buttons added left of the window controls (global search, right
 *   panel toggle) are pure wiring over two stores — there is no layout math
 *   to verify, but plenty of ways to wire them wrong:
 *     1. search button calls `closeDialog` (or toggles), so the first click
 *        never opens the dialog;
 *     2. the panel button reads a *local* copy of `rightPanelCollapsed`
 *        instead of the store, so it shows the wrong icon and the RightNavBar
 *        rows disagree with the title bar;
 *     3. it sets a constant instead of flipping, so the second click is a no-op.
 *
 * What this pins: both buttons drive the same store the rest of the app
 * reads (`searchStore`, `layoutStore.rightPanelCollapsed`), and the panel
 * button round-trips across both states.
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { TitleBar, RightPanelIcon } from "./TitleBar";
import { useSearchStore } from "../../stores/searchStore";
import { useLayoutStore } from "../../stores/layoutStore";
import { useGatewayStore } from "../../stores/gatewayStore";

// TitleBar calls getCurrentWindow() during render; jsdom has no Tauri IPC.
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
  }),
}));

// GatewayStatusChip probes URL history via fetch + Tauri invoke when a
// drop happens; stub both so the mounting tests stay hermetic.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => ({})),
}));

const searchBtn = () => screen.getByRole("button", { name: /Ctrl\+Shift\+F/ });
const panelBtn = () =>
  screen.getByRole("button", { name: /Right Panel/i });

describe("TitleBar view toggles", () => {
  beforeEach(() => {
    useSearchStore.setState({ open: false });
    useLayoutStore.setState({ rightPanelCollapsed: true });
    // ADR-052: the outage chip is steady-state only; start from healthy
    // so the toggle tests below aren't coupled to a gateway outage.
    useGatewayStore.setState({ status: "connected", candidates: [] });
  });

  it("search button opens the global search dialog", () => {
    render(<TitleBar />);
    expect(useSearchStore.getState().open).toBe(false);
    fireEvent.click(searchBtn());
    expect(useSearchStore.getState().open).toBe(true);
  });

  it("search button never closes an open dialog (it is not a toggle)", () => {
    useSearchStore.setState({ open: true });
    render(<TitleBar />);
    fireEvent.click(searchBtn());
    expect(useSearchStore.getState().open).toBe(true);
  });

  it("panel button round-trips both states off the store", () => {
    render(<TitleBar />);
    const set = useLayoutStore.getState().setRightPanelCollapsed;
    // Collapsed on mount -> first click opens.
    fireEvent.click(panelBtn());
    expect(useLayoutStore.getState().rightPanelCollapsed).toBe(false);
    // Open -> second click closes again (guards against a set-constant bug).
    act(() => set(false));
    fireEvent.click(panelBtn());
    expect(useLayoutStore.getState().rightPanelCollapsed).toBe(true);
  });

  it("panel glyph shows fill=open, hollow=closed (no arrow)", () => {
    // State is carried by the right pane's fill, not by swapping in an arrow
    // icon — the arrow reading was rejected as too ambiguous at 14px.
    const { container, rerender } = render(<RightPanelIcon open />);
    // rect[0] = outer frame, rect[1] = right pane (the stateful part)
    const pane = (container.querySelectorAll("rect")[1] as SVGElement);
    expect(pane.getAttribute("fill")).toBe("var(--color-accent)");
    rerender(<RightPanelIcon open={false} />);
    const hollow = (container.querySelectorAll("rect")[1] as SVGElement);
    // No fill attribute at all — it inherits `fill="none"` from the <svg>
    // root, which is what makes the pane hollow. Asserting the *absence*
    // rather than a literal "none" keeps the test honest about the markup.
    expect(hollow.getAttribute("fill")).toBeNull();
  });

  it("panel button tracks the store, not a local snapshot", () => {
    // RightNavBar / RightPanel can collapse the panel on their own; the
    // title-bar icon must follow without the user having re-rendered it.
    act(() => useLayoutStore.setState({ rightPanelCollapsed: false }));
    const { rerender } = render(<TitleBar />);
    expect(panelBtn().getAttribute("aria-pressed")).toBe("true");
    act(() => useLayoutStore.setState({ rightPanelCollapsed: true }));
    rerender(<TitleBar />);
    expect(panelBtn().getAttribute("aria-pressed")).toBe("false");
  });
});

/**
 * The Gateway outage chip is mounted by TitleBar (it replaced the
 * full-width GatewayBanner strip). Worth pinning here because the
 * wiring is invisible in either file alone: TitleBar only renders it
 * because it is imported, and it only appears because AppLayout gated
 * the whole post-boot tree on `gatewayReady`. A dropped import would
 * silently leave the app with no outage indicator at all.
 */
describe("TitleBar Gateway outage chip", () => {
  beforeEach(() => {
    useSearchStore.setState({ open: false });
    useLayoutStore.setState({ rightPanelCollapsed: true });
    useGatewayStore.setState({ status: "disconnected", gatewayUnreachable: true, candidates: [] });
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new Error("down"))));
  });

  it("mounts the chip on a steady-state gateway drop", () => {
    render(<TitleBar />);
    expect(screen.getByRole("button", { expanded: false })).toBeTruthy();
  });

  it("renders no chip while the gateway is healthy", () => {
    act(() => useGatewayStore.setState({ status: "connected", gatewayUnreachable: false }));
    const { container } = render(<TitleBar />);
    expect(container.querySelector('[aria-expanded]')).toBeNull();
  });

  it("stays on the title bar until recovery clears the signal, never auto-hides", () => {
    const { rerender } = render(<TitleBar />);
    expect(screen.queryByRole("button", { expanded: false })).toBeTruthy();
    // Recovery clears it — the connectivity module drops the
    // `gatewayUnreachable` signal on the MQTT rise; nothing else does.
    act(() => useGatewayStore.setState({ gatewayUnreachable: false }));
    rerender(<TitleBar />);
    expect(screen.queryByRole("button", { expanded: false })).toBeNull();
  });
});