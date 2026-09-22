/**
 * Regression test for the gateway URL history feature.
 *
 * History reflects URLs we ACTUALLY connected to (or just disconnected
 * from). It is NOT updated by `setGatewayUrl` — only by `recordGatewayUrl`,
 * which is called from App.tsx's gateway-status subscriber. Typos and
 * unreachable hosts never pollute the list.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useSettingsStore } from "./settingsStore";

vi.mock("@tauri-apps/api/core", () => ({
    invoke: vi.fn(async () => ({})),
}));

describe("settingsStore gateway URL history", () => {
    beforeEach(() => {
        localStorage.clear();
        useSettingsStore.setState({
            gatewayMode: "remote",
            gatewayUrl: "http://192.168.1.10:19876",
            gatewayUrlHistory: [],
        });
        vi.clearAllMocks();
    });

    it("recordGatewayUrl pushes the URL to the front of history", () => {
        useSettingsStore.getState().recordGatewayUrl("http://192.168.1.20:19876");
        expect(useSettingsStore.getState().gatewayUrlHistory[0]).toBe(
            "http://192.168.1.20:19876",
        );
    });

    it("re-recording the same URL is a no-op (no churn, no localStorage write)", () => {
        useSettingsStore.getState().recordGatewayUrl("http://192.168.1.20:19876");
        const setItemSpy = vi.spyOn(Storage.prototype, "setItem");
        useSettingsStore.getState().recordGatewayUrl("http://192.168.1.20:19876");
        // Same value re-recorded must not call setItem for the history key.
        const historyWrites = setItemSpy.mock.calls.filter(
            ([k]) => k === "acowork-gateway-url-history",
        );
        expect(historyWrites.length).toBe(0);
    });

    it("dedupes and caps at 8 entries (LRU)", () => {
        const urls = [
            "http://a:19876",
            "http://b:19876",
            "http://c:19876",
            "http://d:19876",
            "http://e:19876",
            "http://f:19876",
            "http://g:19876",
            "http://h:19876",
            "http://i:19876", // pushes 'a' out
            "http://b:19876", // dedup → moves to front, drops 'i'
        ];
        for (const u of urls) useSettingsStore.getState().recordGatewayUrl(u);
        const hist = useSettingsStore.getState().gatewayUrlHistory;
        expect(hist.length).toBe(8);
        expect(hist[0]).toBe("http://b:19876"); // most recent first
        expect(hist).not.toContain("http://a:19876"); // evicted
        expect(hist.filter((u) => u === "http://b:19876").length).toBe(1);
    });

    it("persists history to localStorage", () => {
        useSettingsStore.getState().recordGatewayUrl("http://10.0.0.1:19876");
        useSettingsStore.getState().recordGatewayUrl("http://10.0.0.2:19876");
        const stored = localStorage.getItem("acowork-gateway-url-history");
        expect(stored).not.toBeNull();
        const parsed = JSON.parse(stored!);
        expect(parsed[0]).toBe("http://10.0.0.2:19876");
        expect(parsed[1]).toBe("http://10.0.0.1:19876");
    });

    it("recordGatewayUrl is idempotent for falsy / duplicate-of-front", () => {
        useSettingsStore.getState().recordGatewayUrl("http://x:19876");
        const first = useSettingsStore.getState().gatewayUrlHistory;
        useSettingsStore.getState().recordGatewayUrl("http://x:19876");
        expect(useSettingsStore.getState().gatewayUrlHistory).toEqual(first);
        useSettingsStore.getState().recordGatewayUrl("");
        useSettingsStore.getState().recordGatewayUrl("   ");
        expect(useSettingsStore.getState().gatewayUrlHistory).toEqual(first);
    });

    it("setGatewayUrl does NOT add to history (history is connection-driven)", () => {
        // The user typed a URL and saved it — that doesn't mean it
        // connected. History must reflect actual connections, not just
        // saved preferences.
        useSettingsStore.getState().setGatewayUrl("http://192.168.17.113:19876");
        useSettingsStore.getState().setGatewayUrl("http://192.168.43.198:19876");
        expect(useSettingsStore.getState().gatewayUrlHistory).toEqual([]);
        // The connection-lifecycle path is the only thing that adds.
        useSettingsStore.getState().recordGatewayUrl("http://192.168.43.198:19876");
        expect(useSettingsStore.getState().gatewayUrlHistory).toEqual([
            "http://192.168.43.198:19876",
        ]);
    });
});