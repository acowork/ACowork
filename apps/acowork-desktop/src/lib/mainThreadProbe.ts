// Temporary diagnostic probes for the "agent start → session content takes
// tens of seconds" report (2026-09-14). Logs to console only (devtools),
// never to disk. ponytail: diagnostic-only — delete after root cause found.

/** Long-task floor in ms. 100ms produced huge false-positive floods from
 *  normal React 18 reconcile + lazy image/icon decode on the Harness tab
 *  (every model row spawns a remote SVG fetch + decode). 250ms keeps the
 *  signal for real stalls while letting the noise pass silently. */
const LONG_TASK_THRESHOLD_MS = 250;

/** Sliding window during which at most one long-task line is emitted.
 *  Prevents a single reconcile storm (root cause: duplicate React keys)
 *  from spamming hundreds of lines before it gets fixed. */
const LONG_TASK_DEDUPE_WINDOW_MS = 2000;

/**
 * 1s heartbeat; warns when the main thread stalls (gap between two ticks
 * exceeds 2s). Normal operation prints nothing. A stall surfaces as one
 * "blocked ~Nms" line — N is the exact stall duration.
 */
export function installMainThreadProbe(tag = "mt-probe"): void {
  let last = performance.now();
  setInterval(() => {
    const now = performance.now();
    const gap = now - last;
    last = now;
    if (gap > 2000) {
      console.warn(
        `[${tag}] main-thread blocked ~${Math.round(gap)}ms ` +
          `(tick ${new Date().toISOString()})`,
      );
    }
  }, 1000);
}

/** Report long tasks (≥250ms) — pinpoints which boot phase stalls.
 *  ponytail: per-tick only the first qualifying entry is emitted; the
 *  rest within `LONG_TASK_DEDUPE_WINDOW_MS` are dropped to keep the
 *  console readable during reconcile storms. Upgrade path: ship a
 *  real sampler (e.g. flamegraph export to Rust debug_log) once a
 *  real stall needs root-causing — this probe is just a canary. */
export function installLongTaskObserver(): void {
  if (typeof PerformanceObserver === "undefined") return;
  try {
    let lastEmittedAt = 0;
    const po = new PerformanceObserver((list) => {
      const entries = list.getEntries();
      const now = performance.now();
      if (now - lastEmittedAt < LONG_TASK_DEDUPE_WINDOW_MS) return;
      const first = entries.find(
        (e) => e.duration > LONG_TASK_THRESHOLD_MS,
      );
      if (!first) return;
      lastEmittedAt = now;
      console.warn(
        `[longtask] ${Math.round(first.duration)}ms ` +
          `start=${Math.round(first.startTime)}ms ` +
          `(${entries.length} task(s) this batch)`,
      );
    });
    po.observe({ entryTypes: ["longtask"] });
  } catch {
    // PerformanceObserver unsupported — probe is best-effort.
  }
}
