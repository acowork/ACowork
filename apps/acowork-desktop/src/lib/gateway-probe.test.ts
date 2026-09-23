/**
 * Self-check for `probeGateways` — the parallel `/health` scanner used
 * by SplashScreen's 5s fallback window.
 *
 * Properties worth pinning:
 *     1. Reachable URLs come back with `ok: true` + latencyMs > 0.
 *     2. Unreachable URLs come back with `ok: false` (no throw).
 *     3. Duplicates in the input collapse to a single probe.
 *     4. An empty / all-duplicate input returns `[]` synchronously.
 *     5. Per-probe timeout is respected: a hanging host doesn't hold the
 *        whole result indefinitely.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { probeGateways } from "./gateway-probe";

describe("probeGateways", () => {
    beforeEach(() => {
        vi.restoreAllMocks();
    });

    it("returns reachable URLs with ok=true and a positive latency", async () => {
        vi.stubGlobal(
            "fetch",
            vi.fn(async () => ({
                ok: true,
                json: async () => ({ status: "ok", version: "0" }),
            })),
        );
        const out = await probeGateways(["http://a:19876", "http://b:19876"]);
        expect(out).toHaveLength(2);
        expect(out.every((r) => r.ok)).toBe(true);
        expect(out.every((r) => r.latencyMs > 0)).toBe(true);
    });

    it("returns unreachable URLs with ok=false and does not throw", async () => {
        vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("ECONNREFUSED"); }));
        const out = await probeGateways(["http://dead:19876"]);
        expect(out).toEqual([{ url: "http://dead:19876", ok: false, latencyMs: 0 }]);
    });

    it("dedupes input URLs", async () => {
        const fetchMock = vi.fn(async () => ({
            ok: true,
            json: async () => ({}),
        }));
        vi.stubGlobal("fetch", fetchMock);
        await probeGateways(["http://a:19876", "http://a:19876", "http://a:19876"]);
        expect(fetchMock).toHaveBeenCalledTimes(1);
    });

    it("returns [] for empty or all-duplicate input", async () => {
        const fetchMock = vi.fn();
        vi.stubGlobal("fetch", fetchMock);
        expect(await probeGateways([])).toEqual([]);
        expect(await probeGateways(["", "   "])).toEqual([]);
        expect(fetchMock).not.toHaveBeenCalled();
    });

    it("respects the per-probe timeout", async () => {
        // fetch never resolves — only an AbortController-driven cancel.
        vi.stubGlobal("fetch", vi.fn((_url: string, init?: { signal?: AbortSignal }) =>
            new Promise((_resolve, reject) => {
                init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
            }),
        ));
        const t0 = performance.now();
        const out = await probeGateways(["http://slow:19876"], { perProbeTimeoutMs: 100 });
        const elapsed = performance.now() - t0;
        expect(out[0].ok).toBe(false);
        expect(elapsed).toBeLessThan(500); // generous bound for CI jitter
    });

    it("mixed results: reachable + unreachable + mixed status codes", async () => {
        vi.stubGlobal(
            "fetch",
            vi.fn(async (url: string) => {
                if (url.includes("404")) return { ok: false, json: async () => ({}) };
                if (url.includes("ok")) return { ok: true, json: async () => ({}) };
                throw new Error("down");
            }),
        );
        const out = await probeGateways([
            "http://ok:19876",
            "http://404:19876",
            "http://down:19876",
        ]);
        const byUrl = Object.fromEntries(out.map((r) => [r.url, r.ok]));
        expect(byUrl["http://ok:19876"]).toBe(true);
        expect(byUrl["http://404:19876"]).toBe(false);
        expect(byUrl["http://down:19876"]).toBe(false);
    });
});