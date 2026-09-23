import { useEffect } from "react";
import { useTranslation } from "../../i18n/useTranslation";
import { useGatewayStore } from "../../stores/gatewayStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { probeGateways } from "../../lib/gateway-probe";
import { Wifi } from "lucide-react";

/**
 * Steady-state Gateway outage banner. Two visual states:
 *   - offline (no candidates): one row, status icon + message + Retry.
 *   - offline + candidates (laptop moved LAN / sleep wake): same row
 *     with candidate pills inline; overflow scrolls horizontally so a
 *     long history list never wraps to a 3rd line.
 *
 * The banner drives its own candidate probe: it runs once on mount
 * (in case App.tsx's transition-based probe missed), and once each
 * time the user clicks Retry (in case the network just came back on
 * a new interface and a fresh scan finds a working address).
 *
 * Visual style mirrors ChatPanel's warn/info containers (translucent
 * bg + 1px border, no flat fill) so it reads as part of the app rather
 * than a system-level OS alert.
 */
export function GatewayBanner() {
  const { t } = useTranslation();
  const checkHealth = useGatewayStore((s) => s.checkHealth);
  const localState = useGatewayStore((s) => s.localState);
  const startLocalGateway = useGatewayStore((s) => s.startLocalGateway);
  const candidates = useGatewayStore((s) => s.candidates);
  const setCandidates = useGatewayStore((s) => s.setCandidates);
  const clearCandidates = useGatewayStore((s) => s.clearCandidates);
  const gatewayMode = useSettingsStore((s) => s.gatewayMode);
  const gatewayUrl = useSettingsStore((s) => s.gatewayUrl);
  const gatewayUrlHistory = useSettingsStore((s) => s.gatewayUrlHistory);
  const setGatewayUrl = useSettingsStore((s) => s.setGatewayUrl);

  const isLocal = gatewayMode === "local";
  const isStarting = localState === "starting";
  const showCandidates = !isLocal && candidates.length > 0;

  /**
   * Banner-mounted probe. Runs in remote mode whenever the banner
   * shows up, regardless of why /health ended up `error`. This is
   * more robust than a status-transition subscriber at the App layer
   * because (a) the banner only exists when the user actually needs
   * to see recovery options, and (b) status transitions can be
   * noisy — /health can pass while the network is effectively dead,
   * or MQTT can disconnect without /health ever being re-checked.
   */
  const runProbe = () => {
    if (gatewayMode !== "remote") return;
    const others = gatewayUrlHistory.filter((u) => u !== gatewayUrl);
    if (others.length === 0) {
      setCandidates([]);
      return;
    }
    void (async () => {
      const results = await probeGateways(others);
      // The user may have hit Retry and the gateway came back during
      // the probe — discard the result in that case so a stale list
      // doesn't show after recovery.
      if (useGatewayStore.getState().status === "connected") {
        setCandidates([]);
        return;
      }
      const reachable = results
        .filter((r) => r.ok)
        .sort((a, b) => a.latencyMs - b.latencyMs)
        .map((r) => ({ url: r.url, latencyMs: r.latencyMs }));
      setCandidates(reachable);
    })();
  };

  useEffect(() => {
    runProbe();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gatewayUrl, gatewayUrlHistory]);

  const onRetry = () => {
    void checkHealth();
    // Re-probe on retry too: a quick network blip might resolve in
    // parallel, and a fresh scan is cheap (≤1.5s per host).
    runProbe();
  };

  const pickCandidate = (url: string) => {
    clearCandidates();
    setGatewayUrl(url);
    void checkHealth();
  };

  // The container style stays warn-tier regardless of candidates —
  // the candidates are an *offer*, not a recovery; the gateway is still
  // unreachable. Translucent so it doesn't fight the title bar above.
  const containerCls =
    "flex items-center gap-3 overflow-x-auto border-b border-amber-300/60 bg-amber-50/80 px-4 py-1.5 text-xs text-amber-900 backdrop-blur-sm dark:border-amber-700/60 dark:bg-amber-950/40 dark:text-amber-100";
  const buttonCls =
    "shrink-0 rounded-md px-2 py-0.5 text-xs font-medium hover:bg-amber-200/60 dark:hover:bg-amber-900/60";
  const ghostCls =
    "shrink-0 text-xs text-amber-700/70 hover:text-amber-900 dark:text-amber-300/70 dark:hover:text-amber-100";

  return (
    <div className={containerCls}>
      <Wifi className="h-3.5 w-3.5 shrink-0 text-amber-500 dark:text-amber-300" />
      <span className="shrink-0 font-medium">
        {isLocal
          ? isStarting
            ? t("splashScreen.bannerLocalStarting")
            : t("splashScreen.bannerLocalDown")
          : t("splashScreen.bannerRemoteDown")}
      </span>

      {showCandidates && (
        <>
          {/* Center group: a short prefix tells the user what the pills
              are ("detected" — alternatives the gateway probe found).
              `mx-auto` slides the whole cluster (prefix + pills) into
              the middle of the banner. */}
          <div className="mx-auto flex shrink-0 items-center gap-1.5">
            <span className="shrink-0 text-[11px] text-amber-700/80 dark:text-amber-300/80">
              {t("splashScreen.candidateDetected")}
            </span>
            {candidates.map((c) => (
              <button
                key={c.url}
                onClick={() => pickCandidate(c.url)}
                title={c.url}
                className="group inline-flex shrink-0 items-center gap-1.5 rounded-full border border-amber-300/80 bg-white/70 px-2.5 py-0.5 font-mono text-[11px] text-amber-900 transition-colors hover:border-amber-400 hover:bg-white dark:border-amber-700/80 dark:bg-zinc-900/40 dark:text-amber-100 dark:hover:border-amber-600 dark:hover:bg-zinc-900"
              >
                <span className="max-w-[14rem] truncate">{c.url}</span>
                <span className="text-[10px] opacity-60 group-hover:opacity-100">
                  {Math.round(c.latencyMs)}ms
                </span>
              </button>
            ))}
          </div>
        </>
      )}

      {/* Right group: secondary actions. When there are no candidates
          `ml-auto` slides this group to the right edge so the banner
          stays balanced (label left, action right). When there ARE
          candidates the middle pill group already ate the slack, so
          `ml-auto` is a no-op here. */}
      <div className="ml-auto flex shrink-0 items-center gap-2">
        {showCandidates && (
          <button onClick={clearCandidates} className={ghostCls}>
            {t("splashScreen.candidateKeepWaiting")}
          </button>
        )}
        {isLocal && !isStarting && (
          <button onClick={startLocalGateway} className={buttonCls}>
            {t("splashScreen.bannerStart")}
          </button>
        )}
        <button onClick={onRetry} className={buttonCls}>
          {t("splashScreen.bannerRetry")}
        </button>
      </div>
    </div>
  );
}
