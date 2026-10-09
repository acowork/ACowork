/**
 * Tests for the local-network auto-heal layer of the connectivity
 * module — the scenario it exists for: a laptop wake-up / Wi-Fi hop
 * leaves the persisted URL on the OLD local IP (a black hole) while
 * the Gateway keeps listening on loopback and on the new IP.
 *
 * Pinned here:
 *   - pure helpers (`isLoopbackHost`, `replaceUrlHost`);
 *   - hint recording when the URL host is among this machine's IPs;
 *   - silent switch to loopback when the host stops being local
 *     (loopback preferred even when a LAN IP answers too);
 *   - safety: a genuinely remote gateway (hint mismatch) is untouched;
 *   - backoff: when nothing answers, the guard yields to the normal
 *     unreachable hint + candidates flow.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const invokeMock = vi.fn(async (_cmd: string, ..._args: unknown[]) => ({}));
vi.mock("@tauri-apps/api/core", () => ({
    invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

vi.mock("../logger", () => ({
    log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
    setLevel: () => {},
    getLevel: () => "off" as const,
}));

import {
    _resetLocalNetworkForTests,
    isLoopbackHost,
    maybeAutoSwitchLocalGateway,
    readLocalHostHint,
    replaceUrlHost,
} from "./localNetwork";
import { useChatStore } from "../../stores/chatStore";
import { useSettingsStore } from "../../stores/settingsStore";

const OLD_LOCAL = "http://192.168.0.101:19876";
const NEW_IP = "192.168.17.113";
const NEW_URL = `http://${NEW_IP}:19876`;
const LOOPBACK_URL = "http://127.0.0.1:19876";
const REMOTE_URL = "http://10.9.9.9:19876";
const STORAGE_KEY = "acowork-gateway-local-host-hint";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

async function flushMicrotasks() {
    for (let i = 0; i < 10; i++) await Promise.resolve();
}

/** `get_local_ipv4_addresses` returns `ips`; every other command a no-op. */
function stubLocalIps(ips: string[]) {
    invokeMock.mockImplementation(async (cmd: string) =>
        cmd === "get_local_ipv4_addresses" ? ips : {},
    );
}

beforeEach(() => {
    _resetLocalNetworkForTests();
    localStorage.clear();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async () => ({}));
    fetchMock.mockReset();
    // Only loopback answers /health — the classic "Gateway runs on this
    // machine, old LAN IP is a black hole" shape.
    fetchMock.mockImplementation((url: string) => {
        if (String(url).includes("127.0.0.1")) {
            return Promise.resolve({ ok: true, json: async () => ({ status: "ok" }) } as Response);
        }
        return Promise.reject(new Error("unreachable"));
    });
    useChatStore.setState({ mqttConnected: false });
    useSettingsStore.setState({
        gatewayMode: "remote",
        gatewayUrl: OLD_LOCAL,
        gatewayUrlHistory: [OLD_LOCAL],
    });
});

describe("pure helpers", () => {
    it("isLoopbackHost covers 127.0.0.1 / localhost / ::1", () => {
        expect(isLoopbackHost("127.0.0.1")).toBe(true);
        expect(isLoopbackHost("localhost")).toBe(true);
        expect(isLoopbackHost("::1")).toBe(true);
        expect(isLoopbackHost("[::1]")).toBe(true);
        expect(isLoopbackHost("192.168.0.101")).toBe(false);
    });

    it("replaceUrlHost keeps scheme and port, null on garbage", () => {
        expect(replaceUrlHost("https://relay.example.com", "127.0.0.1")).toBe(
            "https://127.0.0.1",
        );
        expect(replaceUrlHost(OLD_LOCAL, NEW_IP)).toBe(NEW_URL);
        expect(replaceUrlHost("not a url", "127.0.0.1")).toBeNull();
    });
});

describe("maybeAutoSwitchLocalGateway", () => {
    it("records the hint while the URL host is one of this machine's IPs", async () => {
        stubLocalIps(["192.168.0.101", NEW_IP]);
        await maybeAutoSwitchLocalGateway();
        expect(readLocalHostHint()).toBe("192.168.0.101");
        // Connected shape — nothing may be switched.
        expect(useSettingsStore.getState().gatewayUrl).toBe(OLD_LOCAL);
        expect(fetchMock).not.toHaveBeenCalled();
    });

    it("Wi-Fi hop: stale local host → silently switches to loopback", async () => {
        localStorage.setItem(STORAGE_KEY, "192.168.0.101");
        stubLocalIps([NEW_IP]);
        await maybeAutoSwitchLocalGateway();
        await flushMicrotasks();
        expect(useSettingsStore.getState().gatewayUrl).toBe(LOOPBACK_URL);
    });

    it("leaves a genuinely remote gateway alone (hint mismatch)", async () => {
        useSettingsStore.setState({ gatewayUrl: REMOTE_URL });
        stubLocalIps([NEW_IP]);
        await maybeAutoSwitchLocalGateway();
        expect(useSettingsStore.getState().gatewayUrl).toBe(REMOTE_URL);
        expect(fetchMock).not.toHaveBeenCalled();
    });

    it("does not touch a connection that is already up", async () => {
        localStorage.setItem(STORAGE_KEY, "192.168.0.101");
        useChatStore.setState({ mqttConnected: true });
        stubLocalIps([NEW_IP]);
        await maybeAutoSwitchLocalGateway();
        expect(useSettingsStore.getState().gatewayUrl).toBe(OLD_LOCAL);
        expect(fetchMock).not.toHaveBeenCalled();
    });

    it("skips relay mode entirely", async () => {
        useSettingsStore.setState({
            gatewayMode: "relay",
            gatewayUrl: "https://device.relay.acowork.ai",
        });
        await maybeAutoSwitchLocalGateway();
        expect(invokeMock).not.toHaveBeenCalled();
    });

    it("loopback URL is exempt (it is the preferred destination, not a foreign host)", async () => {
        useSettingsStore.setState({ gatewayUrl: LOOPBACK_URL });
        await maybeAutoSwitchLocalGateway();
        expect(invokeMock).not.toHaveBeenCalled();
    });

    it("backs off when nothing answers, yielding to the unreachable flow", async () => {
        localStorage.setItem(STORAGE_KEY, "192.168.0.101");
        fetchMock.mockImplementation(() => Promise.reject(new Error("down")));
        stubLocalIps([NEW_IP]);
        await maybeAutoSwitchLocalGateway();
        await flushMicrotasks();
        expect(useSettingsStore.getState().gatewayUrl).toBe(OLD_LOCAL);
        // Cooldown: the next pass must not even enumerate interfaces.
        invokeMock.mockClear();
        await maybeAutoSwitchLocalGateway();
        expect(invokeMock).not.toHaveBeenCalled();
    });
});
