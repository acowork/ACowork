/**
 * GatewaySwitcherMenu — the title-bar "minimized Settings ▸ Gateway".
 *
 * What this pins (each was a real way to get it wrong):
 *   1. The mode listbox is a FILTER, not a config editor. Selecting
 *      "local" must NOT call `setGatewayMode` — that pushes to Rust and
 *      rebuilds MQTT, which would switch the connection out from under
 *      the user with no address chosen.
 *   2. Picking a row switches through `applyGatewayUrl` + `checkHealth`
 *      (the outage chip's entry point), NOT `setGatewayUrl`. `setGatewayUrl`
 *      alone skips the relay→remote correction and the auth re-validation.
 *   3. The list is `gatewayUrlHistory` (URLs that actually connected) with
 *      the current URL forced to the front — a first-run install has a URL
 *      but no history, and the panel must not be empty then.
 *   4. Probing is per-row and local; the panel must not write into
 *      `gatewayStore.candidates`, which the outage chip owns.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { GatewaySwitcherMenu } from "./GatewaySwitcherMenu";
import { useSettingsStore } from "../../stores/settingsStore";
import { useGatewayStore } from "../../stores/gatewayStore";
import { probeGateways } from "../../lib/gateway-probe";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));

// Probe is the network; stub it and record what it was asked to check.
const probeMock = vi.hoisted(() => vi.fn(async () => [] as { url: string; ok: boolean; latencyMs: number }[]));
vi.mock("../../lib/gateway-probe", () => ({ probeGateways: probeMock }));

const openBtn = () => screen.getByRole("button", { name: /Gateway|网关|閘道|ゲートウェイ|게이트웨이/ });

const open = () => {
  render(<GatewaySwitcherMenu />);
  fireEvent.click(openBtn());
};

describe("GatewaySwitcherMenu", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    probeMock.mockImplementation(async () => []);
    useSettingsStore.setState({
      gatewayMode: "remote",
      gatewayUrl: "http://192.168.1.10:19876",
      gatewayUrlHistory: ["http://192.168.1.10:19876", "http://192.168.1.20:19876"],
    });
    useGatewayStore.setState({ status: "connected", localState: "running", candidates: [] });
  });

  it("lists the current URL first, then recorded history", () => {
    open();
    const rows = screen.getAllByRole("button", { name: /19876/ });
    expect(rows[0].getAttribute("title")).toBe("http://192.168.1.10:19876");
    expect(screen.getByRole("button", { name: /192\.168\.1\.20/ })).toBeTruthy();
  });

  it("mode listbox filters the list WITHOUT changing the configured mode", () => {
    const setSpy = vi.spyOn(useSettingsStore.getState(), "setGatewayMode");
    open();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "local" } });
    // Filtered: local shows only the loopback/current address, with the
    // lifecycle row instead of the browsable history.
    expect(screen.getByText("http://192.168.1.10:19876")).toBeTruthy();
    expect(screen.queryByText("http://192.168.1.20:19876")).toBeNull();
    // ...and the live config was NOT committed.
    expect(setSpy).not.toHaveBeenCalled();
    expect(useSettingsStore.getState().gatewayMode).toBe("remote");
    setSpy.mockRestore();
  });

  it("clicking another address switches via applyGatewayUrl + checkHealth", () => {
    const applySpy = vi.spyOn(useSettingsStore.getState(), "applyGatewayUrl");
    const checkSpy = vi.spyOn(useGatewayStore.getState(), "checkHealth").mockResolvedValue();
    open();
    fireEvent.click(screen.getByRole("button", { name: /192\.168\.1\.20/ }));
    expect(applySpy).toHaveBeenCalledWith("http://192.168.1.20:19876");
    expect(checkSpy).toHaveBeenCalled();
    applySpy.mockRestore();
    checkSpy.mockRestore();
  });

  it("probes the browsed list on open and does NOT touch gatewayStore.candidates", async () => {
    probeMock.mockImplementation(async (urls: string[], opts?: { onSettled?: (r: never) => void }) => {
      urls.forEach((u) => opts?.onSettled?.({ url: u, ok: u.endsWith(".20:19876"), latencyMs: 3 } as never));
      return [];
    });
    open();
    await waitFor(() => expect(probeMock).toHaveBeenCalled());
    expect(probeMock.mock.calls[0][0]).toEqual([
      "http://192.168.1.10:19876",
      "http://192.168.1.20:19876",
    ]);
    // The outage chip owns this slice — borrowing it would leak this
    // menu's rows into the failover banner.
    expect(useGatewayStore.getState().candidates).toEqual([]);
  });

  it("does not probe in local mode (single loopback address)", () => {
    useSettingsStore.setState({ gatewayMode: "local" });
    open();
    expect(probeMock).not.toHaveBeenCalled();
  });

  it("closes on Escape", () => {
    open();
    expect(screen.getByRole("button", { name: /192\.168\.1\.20/ })).toBeTruthy();
    act(() => {
      fireEvent.keyDown(document, { key: "Escape" });
    });
    expect(screen.queryByRole("button", { name: /192\.168\.1\.20/ })).toBeNull();
  });
});