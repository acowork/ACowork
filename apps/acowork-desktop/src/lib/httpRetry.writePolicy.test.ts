/**
 * WRITE_503_RETRY sizing contract.
 *
 * `WRITE_503_RETRY` exists to keep a user-initiated write (session control
 * op, memory-node delete) from freezing for the 60s read budget, and its
 * numbers are easy to "tidy" into a broken state without any test noticing:
 *
 *   - The Gateway hard-codes `Retry-After: 2` on every boot-window 503
 *     (`proxy.rs` / `auth_middleware.rs`), and `with503Retry` lets the header
 *     OVERRIDE `backoffBaseMs`. So the real cadence is a flat 2s and
 *     `backoffBaseMs` is only a fallback for a 503 with no header.
 *   - `with503Retry` checks `totalBudgetMs` BEFORE the attempt count. A
 *     budget below 4 x 2s therefore kills the loop early — the old
 *     `5 retries / 500ms base / 6s budget` combination died at t=6s after
 *     only 2 retries and logged a spurious "exceeded retry budget" warning.
 *
 * These tests pin the *observable* behaviour (retry count, elapsed time)
 * rather than restating the constants, so a future retune that regresses
 * either property fails here.
 */

import { describe, it, expect, vi, afterEach } from "vitest";
import { with503Retry, WRITE_503_RETRY } from "./httpRetry";

/** Minimal Response stub — only what with503Retry touches. */
function mockResp(status: number, retryAfter: string | null): Response {
    const headers = new Headers();
    if (retryAfter !== null) headers.set("retry-after", retryAfter);
    return {
        status,
        ok: status >= 200 && status < 300,
        headers,
        json: () => Promise.resolve({}),
    } as unknown as Response;
}

afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
});

describe("WRITE_503_RETRY", () => {
    it("rides out a full Retry-After: 2 cadence without tripping the budget", async () => {
        vi.useFakeTimers();
        // 4 retries x 2s of header-forced sleep = 8s of wall clock, which
        // must stay under totalBudgetMs or the loop exits early.
        const elapsed = WRITE_503_RETRY.maxRetries * 2_000;
        expect(elapsed).toBeLessThan(WRITE_503_RETRY.totalBudgetMs);

        // The Gateway never stops 503-ing here: we assert the loop gives up
        // on the retry count (not the budget) after exactly maxRetries+1
        // attempts, and that no budget-exceeded warning was logged.
        const fetchMock = vi.fn(async () => mockResp(503, "2"));
        vi.stubGlobal("fetch", fetchMock);
        const warn = vi.fn();
        const debug = vi.fn();

        const promise = with503Retry(() => fetch("x"), {
            policy: WRITE_503_RETRY,
            logger: { warn, debug },
        });
        await vi.advanceTimersByTimeAsync(WRITE_503_RETRY.totalBudgetMs + 1_000);
        const resp = await promise;

        expect(resp.status).toBe(503);
        // 1 initial attempt + maxRetries replays.
        expect(fetchMock).toHaveBeenCalledTimes(WRITE_503_RETRY.maxRetries + 1);
        // Exiting on the attempt counter is the intended path; the budget
        // branch must stay silent or it warns into the desktop log file.
        expect(warn.mock.calls.flat().join(" ")).not.toContain("exceeded retry budget");
        expect(warn.mock.calls.flat().join(" ")).toContain("exhausted");
    });

    it("stops well before the 60s read budget so a dead Runtime surfaces fast", async () => {
        vi.useFakeTimers();
        const fetchMock = vi.fn(async () => mockResp(503, null));
        vi.stubGlobal("fetch", fetchMock);

        // No Retry-After header -> the exponential backoff fallback applies
        // (2s, 4s, 4s, ... capped at backoffCapMs), so the loop leaves via the
        // wall-clock budget rather than the attempt count. Advance past the
        // worst-case backoff so the fake clock can't strand the promise.
        const promise = with503Retry(() => fetch("x"), { policy: WRITE_503_RETRY });
        await vi.advanceTimersByTimeAsync(60_000);
        const resp = await promise;

        expect(resp.status).toBe(503);
        // 1 initial attempt + maxRetries replays, never more.
        expect(fetchMock.mock.calls.length).toBeLessThanOrEqual(WRITE_503_RETRY.maxRetries + 1);
        // The whole point of a separate write policy: a stuck write must not
        // occupy the UI for the 60s the read paths are allowed to wait.
        expect(WRITE_503_RETRY.totalBudgetMs).toBeLessThanOrEqual(15_000);
    });
});
