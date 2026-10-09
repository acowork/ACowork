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

    /**
     * The latency property this whole `onSettled` hook exists for: a live
     * host must not wait on a black-holed one burning its 1.5s budget.
     * Measured before the fix at 1580ms; the live host answered in ~2ms.
     */
    it("onSettled delivers the live host long before the batch resolves", async () => {
        vi.stubGlobal(
            "fetch",
            vi.fn((url: string, init?: { signal?: AbortSignal }) => {
                if (url.includes("live")) {
                    return Promise.resolve({ ok: true, json: async () => ({}) } as Response);
                }
                // Black hole — must honour the abort signal like real fetch.
                return new Promise<Response>((_resolve, reject) => {
                    init?.signal?.addEventListener("abort", () =>
                        reject(new DOMException("aborted", "AbortError")),
                    );
                });
            }),
        );
        const settledAt: Record<string, number> = {};
        const t0 = performance.now();
        await probeGateways(
            ["http://blackhole:19876", "http://live:19876"],
            {
                perProbeTimeoutMs: 800,
                onSettled: (r) => {
                    settledAt[r.url] = performance.now() - t0;
                },
            },
        );
        // The live host is offered to the caller essentially immediately…
        expect(settledAt["http://live:19876"]).toBeLessThan(300);
        // …while the black hole only settles once its budget expires.
        expect(settledAt["http://blackhole:19876"]).toBeGreaterThanOrEqual(700);
    });

    it("onSettled is optional and does not change the returned batch", async () => {
        vi.stubGlobal(
            "fetch",
            vi.fn(async (url: string) =>
                url.includes("ok")
                    ? ({ ok: true, json: async () => ({}) } as Response)
                    : ({ ok: false, json: async () => ({}) } as Response),
            ),
        );
        const withHook = await probeGateways(["http://ok:19876", "http://bad:19876"], {
            onSettled: () => {},
        });
        const withoutHook = await probeGateways(["http://ok:19876", "http://bad:19876"]);
        expect(withHook.map((r) => r.ok)).toEqual(withoutHook.map((r) => r.ok));
        expect(withHook).toHaveLength(2);
    });
});