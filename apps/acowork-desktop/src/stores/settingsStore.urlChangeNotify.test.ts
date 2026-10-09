/**
 * Regression test for the WiFi-hop / LAN-move auth bug (67 → 61 incident,
 * then the WiFi-switch follow-up).
 *
 * When the user saves a new Gateway address — SplashScreen timeout view,
 * GatewayStatusChip candidate pick, OnboardingFlow edit, or the Settings page
 * — `setGatewayUrl` must notify the authStore so the session can be
 * re-validated against the new endpoint:
 *
 *   - Same Gateway behind an alias (127.0.0.1 → localhost) → session kept
 *   - Different Gateway (new signing key)                  → session dropped
 *   - Network unreachable                                 → session kept,
 *                                                            natural 401
 *                                                            ladder recovers
 *
 * Before the fix, the authStore kept its old token pair after a URL
 * change; every `/api/*` answered 401 from the new Gateway, the fetch
 * interceptor's refresh path also failed (refresh_token bound to the
 * old Gateway's signing key), and the user was stranded on what looked
 * like a working app with a broken Node panel ("Node 不在网关里显示").
 *
 * The authStore side of the contract is tested separately in
 * `authStore.test.ts` (`authStore.onGatewayUrlChanged`). This file only
 * covers the settingsStore side: that `setGatewayUrl` actually fires
 * the notification when the URL changes, and skips it on a no-op write.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";

// vi.hoisted ensures the spy exists before vi.mock factory captures it
// — otherwise the factory would close over an undefined reference
// because vi.mock is hoisted above the const declarations.
const { mockOnGatewayUrlChanged } = vi.hoisted(() => ({
    mockOnGatewayUrlChanged: vi.fn(async (_newUrl: string, _oldUrl: string) => {}),
}));

vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => ({})),
}));

// Stub authStore's onGatewayUrlChanged so settingsStore sees a real
// callable without dragging the rest of the auth machinery into the
// test. The authStore tests cover the actual probe decision tree.
vi.mock("./authStore", () => ({
    useAuthStore: {
        getState: () => ({
            onGatewayUrlChanged: mockOnGatewayUrlChanged,
        }),
    },
}));

import { useSettingsStore } from "./settingsStore";
import { useAuthStore } from "./authStore";

describe("settingsStore.setGatewayUrl → authStore notification", () => {
    beforeEach(() => {
        vi.clearAllMocks();
        localStorage.clear();
        // Pin the starting URL + mode so the "changed" assertion is
        // unambiguous. The mode also matters for the Rust-config
        // regression guard (pushGatewayConfigToRust forwards mode
        // into set_gateway_config).
        useSettingsStore.setState({
            gatewayUrl: "http://192.168.3.61:19876",
            gatewayMode: "remote",
        });
    });

    it("calls authStore.onGatewayUrlChanged when the URL actually changes", () => {
        useSettingsStore.getState().setGatewayUrl("http://192.168.3.99:19876");

        expect(mockOnGatewayUrlChanged).toHaveBeenCalledTimes(1);
        expect(mockOnGatewayUrlChanged).toHaveBeenCalledWith(
            "http://192.168.3.99:19876",
            "http://192.168.3.61:19876",
        );
    });

    it("does not call authStore.onGatewayUrlChanged on a no-op write", () => {
        const current = useSettingsStore.getState().gatewayUrl;

        useSettingsStore.getState().setGatewayUrl(current);

        expect(mockOnGatewayUrlChanged).not.toHaveBeenCalled();
    });

    it("still pushes the Rust config when the URL changes (regression guard)", async () => {
        useSettingsStore.getState().setGatewayUrl("http://192.168.3.99:19876");

        // pushGatewayConfigToRust runs set_gateway_config + connect_mqtt;
        // the authStore notification must not skip or replace it. Both
        // invokes are async, so wait for the second one (connect_mqtt is
        // issued after set_gateway_config resolves inside the helper).
        await vi.waitFor(() => {
            const commands = vi.mocked(invoke).mock.calls.map(([cmd]) => String(cmd));
            expect(commands).toEqual(["set_gateway_config", "connect_mqtt"]);
        });
        expect(invoke).toHaveBeenCalledWith("set_gateway_config", {
            config: { mode: "remote", url: "http://192.168.3.99:19876" },
        });
    });

    // Sanity check that the hoisted mock is actually the same object the
    // settingsStore sees — protects against a future refactor that
    // accidentally introduces two mock layers.
    it("the hoisted mock is reachable via useAuthStore.getState()", () => {
        expect(useAuthStore.getState().onGatewayUrlChanged).toBe(mockOnGatewayUrlChanged);
    });
});