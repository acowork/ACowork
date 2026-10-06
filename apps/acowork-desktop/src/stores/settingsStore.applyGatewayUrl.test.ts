/**
 * Unit tests for `applyGatewayUrl` — the URL+mode atomic-consistency
 * action.
 *
 * Incident context: the SplashScreen 5s fallback surfaced a reachable
 * http LAN candidate while the mode was still `relay`. The old pick path
 * only called `setGatewayUrl`, leaving the illegal `relay` + `http://`
 * combo: HTTP health probes answered (so the Gateway LOOKED alive) but
 * every MQTT CONNECT was rejected by the Rust-side transport check
 * (`relay_mqtt_wss_url` only accepts https device domains) — the chat
 * was permanently dead with no visible error.
 *
 * Contract:
 *  - relay + http://  → mode flips to `remote`; the LAST
 *    `set_gateway_config` push is (remote, url).
 *  - relay + https:// → no mode change (combo is legal).
 *  - remote + http:// → no mode change.
 *  - same URL, no fix needed → complete no-op (no pushes).
 *  - same URL but relay + http:// (the broken persisted state) →
 *    mode-only self-heal push (remote, url).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useSettingsStore } from "./settingsStore";

vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => ({})),
}));

const RELAY_URL = "https://c8cb2bed-ffd4-4821-a913-718618482157.relay.acowork.ai";
const OTHER_RELAY_URL = "https://d1e2f3a4-1111-2222-3333-444455556666.relay.acowork.ai";
const LAN_URL = "http://192.168.0.101:19876";
const OLD_LAN_URL = "http://192.168.1.10:19876";

/** Config payloads passed to `set_gateway_config`, in call order. */
function configPushes(): { mode: string; url: string }[] {
    return vi.mocked(invoke).mock.calls
        .filter(([cmd]) => cmd === "set_gateway_config")
        .map(([, args]) => (args as { config: { mode: string; url: string } }).config);
}

/** The config payload of the LAST `set_gateway_config` call. */
function lastConfigPush(): { mode: string; url: string } | undefined {
    const pushes = configPushes();
    return pushes[pushes.length - 1];
}

describe("settingsStore.applyGatewayUrl", () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it("relay + http URL → flips mode to remote and ends on a (remote, url) push", async () => {
        useSettingsStore.setState({ gatewayMode: "relay", gatewayUrl: RELAY_URL });

        useSettingsStore.getState().applyGatewayUrl(LAN_URL);

        expect(useSettingsStore.getState().gatewayUrl).toBe(LAN_URL);
        expect(useSettingsStore.getState().gatewayMode).toBe("remote");
        await vi.waitFor(() => {
            expect(lastConfigPush()).toEqual({ mode: "remote", url: LAN_URL });
        });
        // The remote+http combo must never be followed by another push
        // that regresses it (the intermediate URL push carries the old
        // relay mode — only the LAST push is authoritative).
        const pushes = configPushes();
        expect(pushes[pushes.length - 1]).toEqual({ mode: "remote", url: LAN_URL });
    });

    it("relay + https URL → keeps relay mode (no mode-fix push)", async () => {
        useSettingsStore.setState({ gatewayMode: "relay", gatewayUrl: RELAY_URL });

        useSettingsStore.getState().applyGatewayUrl(OTHER_RELAY_URL);

        expect(useSettingsStore.getState().gatewayMode).toBe("relay");
        await vi.waitFor(() => {
            expect(lastConfigPush()).toEqual({ mode: "relay", url: OTHER_RELAY_URL });
        });
        // Exactly one push — the URL change; no follow-up mode fix.
        expect(configPushes()).toHaveLength(1);
    });

    it("remote + http URL → keeps remote mode (no mode-fix push)", async () => {
        useSettingsStore.setState({ gatewayMode: "remote", gatewayUrl: OLD_LAN_URL });

        useSettingsStore.getState().applyGatewayUrl(LAN_URL);

        expect(useSettingsStore.getState().gatewayMode).toBe("remote");
        await vi.waitFor(() => {
            expect(lastConfigPush()).toEqual({ mode: "remote", url: LAN_URL });
        });
        expect(configPushes()).toHaveLength(1);
    });

    it("same URL with a legal combo → complete no-op (no pushes)", () => {
        useSettingsStore.setState({ gatewayMode: "remote", gatewayUrl: LAN_URL });

        useSettingsStore.getState().applyGatewayUrl(LAN_URL);

        expect(useSettingsStore.getState().gatewayMode).toBe("remote");
        expect(configPushes()).toHaveLength(0);
    });

    it("same URL in the broken relay+http state → self-heals the mode to remote", async () => {
        useSettingsStore.setState({ gatewayMode: "relay", gatewayUrl: LAN_URL });

        useSettingsStore.getState().applyGatewayUrl(LAN_URL);

        expect(useSettingsStore.getState().gatewayUrl).toBe(LAN_URL);
        expect(useSettingsStore.getState().gatewayMode).toBe("remote");
        await vi.waitFor(() => {
            expect(lastConfigPush()).toEqual({ mode: "remote", url: LAN_URL });
        });
        // URL unchanged → no redundant URL push; only the mode fix.
        expect(configPushes()).toHaveLength(1);
    });
});
