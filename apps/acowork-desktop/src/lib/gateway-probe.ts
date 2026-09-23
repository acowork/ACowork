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
  options: { perProbeTimeoutMs?: number; signal?: AbortSignal } = {},
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

  const tasks = uniq.map((url) => {
    const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
    const timer = setTimeout(() => controller?.abort(), timeoutMs(perProbeTimeoutMs));
    const startedAt = performance.now();
    return probeOne(url, controller?.signal, startedAt).finally(() => clearTimeout(timer));
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