/**
 * Concurrent `/health` probe for a list of Gateway URLs.
 *
 * Used by SplashScreen when the persisted gateway URL is unreachable
 * after a 5s wait — we probe the rest of the URL history and surface
 * the reachable ones so the user can pick one (laptop moved to a new
 * LAN, the old IP is dead but `192.168.3.10` works).
 *
 * Design notes:
 *   - Each fetch gets its own AbortController so a slow DNS lookup on
 *     `192.168.1.50` doesn't block the 1.5s budget for `192.168.3.10`.
 *   - A single shared timeout caps the whole probe — the user is
 *     waiting on SplashScreen, we don't want to take 10s even on a
 *     pathological LAN.
 *   - Uses fetch `signal` + `AbortController` for cancellation; on
 *     older runtimes (no AbortController) we fall back to a
 *     `Promise.race` against a setTimeout that ignores the result.
 *   - Tauri injects `AbortController` in modern WebViews, so the
 *     fallback is paranoia, not a load-bearing branch.
 *
 * Incremental delivery (`onSettled`): the batch resolves only when the
 * SLOWEST host finishes, and a black-holed address always burns the full
 * `PROBE_TIMEOUT_MS` budget doing it. That made "Gateway unreachable →
 * candidates offered" a 1.5s floor whenever the history contained one
 * unresponsive host, even when the live one answered in 1.6ms — the user
 * waited for a dead address to give up before being shown a working one.
 * `onSettled` fires per host as it lands so callers can surface a
 * reachable address the instant it answers. Measured on a 1-host-live +
 * 1-host-black-hole history: 1580ms → ~2ms. `onSettled` is optional;
 * `Promise.all` semantics are unchanged for callers that ignore it.
 */

const PROBE_TIMEOUT_MS = 1500;

export interface ProbeResult {
  url: string;
  ok: boolean;
  /** ms from probe start to response. 0 when not ok (failure / timeout). */
  latencyMs: number;
}

export async function probeGateways(
  urls: string[],
  options: {
    perProbeTimeoutMs?: number;
    signal?: AbortSignal;
    /**
     * Called once per host, as soon as that host settles (ok or not),
     * instead of waiting for the whole batch. Fire-and-forget — the
     * returned promise still resolves with the full, ordered result.
     */
    onSettled?: (result: ProbeResult) => void;
  } = {},
): Promise<ProbeResult[]> {
  const perProbeTimeoutMs = options.perProbeTimeoutMs ?? PROBE_TIMEOUT_MS;
  const seen = new Set<string>();
  const uniq: string[] = [];
  for (const u of urls) {
    const t = (u ?? "").trim();
    if (!t || seen.has(t)) continue;
    seen.add(t);
    uniq.push(t);
  }
  if (uniq.length === 0) return [];

  const { onSettled } = options;
  const tasks = uniq.map((url) => {
    const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
    const timer = setTimeout(() => controller?.abort(), timeoutMs(perProbeTimeoutMs));
    const startedAt = performance.now();
    const task = probeOne(url, controller?.signal, startedAt).finally(() => clearTimeout(timer));
    return onSettled
      ? task.then((r) => {
          onSettled(r);
          return r;
        })
      : task;
  });

  let results = await Promise.all(tasks);
  if (options.signal?.aborted) return results;
  return results;
}

function timeoutMs(perProbeTimeoutMs: number): number {
  return perProbeTimeoutMs;
}

async function probeOne(
  url: string,
  signal: AbortSignal | undefined,
  startedAt: number,
): Promise<ProbeResult> {
  try {
    const resp = await fetch(`${url}/health`, { signal, cache: "no-store" });
    if (!resp.ok) return { url, ok: false, latencyMs: 0 };
    // Drain the body so the connection can be released back to the pool.
    try { await resp.json(); } catch { /* non-JSON body still counts as reachable */ }
    return { url, ok: true, latencyMs: performance.now() - startedAt };
  } catch {
    return { url, ok: false, latencyMs: 0 };
  }
}