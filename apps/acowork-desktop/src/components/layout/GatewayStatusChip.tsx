import { useEffect, useRef, useState } from "react";
import { ChevronDown, Wifi } from "lucide-react";
import { useTranslation } from "../../i18n/useTranslation";
import { useGatewayStore } from "../../stores/gatewayStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { probeKnownCandidates } from "../../lib/connectivity/gatewayConnectivity";
import { cn } from "../../lib/utils";

/**
 * Gateway status chip — title-bar resident, NOT a full-width banner.
 *
 * WHY a title-bar chip (this replaced the old amber `<GatewayBanner />`
 * strip that sat between the title bar and the content):
 *   - A gateway drop after a sleep/wake network switch is a *steady
 *     state*, not a transient toast. It stays on screen until the user
 *     acts, so the affordance must be permanent too — a dot that fades
 *     out would under-report a problem that is still there.
 *   - The strip pushed a 36px amber band across the top of every view
 *     and out-shouted the actual content, for an issue carrying one
 *     short sentence. A chip keeps the attention (solid amber fill +
 *     breathing icon + candidate count badge) while costing ~24px of a
 *     32px title bar and zero content height.
 *
 * Visibility is `gatewayUnreachable` — the connectivity module's
 * user-facing signal for "not CONNACKed for 5s, whatever the cause"
 * (lib/connectivity/gatewayConnectivity.ts). It is deliberately NOT
 * `status === "error"`: the death classifier can stay silent on a
 * black-holed path, which is exactly the outage the user must see.
 * SplashScreen owns every transient state, so there is no boot-window
 * flicker.
 *
 * The candidate probe is the module's shared `probeKnownCandidates` —
 * the same one SplashScreen's fallback uses: once on becoming visible,
 * and once per Retry click (the network may have just come back).
 */
export function GatewayStatusChip() {
  const { t } = useTranslation();
  const gatewayUnreachable = useGatewayStore((s) => s.gatewayUnreachable);
  const checkHealth = useGatewayStore((s) => s.checkHealth);
  const localState = useGatewayStore((s) => s.localState);
  const startLocalGateway = useGatewayStore((s) => s.startLocalGateway);
  const candidates = useGatewayStore((s) => s.candidates);
  const clearCandidates = useGatewayStore((s) => s.clearCandidates);
  const gatewayMode = useSettingsStore((s) => s.gatewayMode);
  const applyGatewayUrl = useSettingsStore((s) => s.applyGatewayUrl);

  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  const visible = gatewayUnreachable;
  const isLocal = gatewayMode === "local";
  const isStarting = localState === "starting";

  // Close the popover on an outside click / Escape so it never traps
  // pointer events over the app behind it.
  useEffect(() => {
    if (!open) return;
    const onDocDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDocDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  /**
   * Candidate probe, shared with SplashScreen's boot fallback through
   * the connectivity module: folds results in per host as each settles
   * (a black-holed address burns the full 1.5s budget — waiting for
   * the batch kept a 2ms host invisible for over a second and a half).
   */
  const runProbe = () => {
    void probeKnownCandidates();
  };

  useEffect(() => {
    if (!visible) return;
    runProbe();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible]);

  if (!visible) return null;

  const onRetry = () => {
    void checkHealth();
    // Re-probe on retry too: a quick network blip might resolve in
    // parallel, and a fresh scan is cheap (≤1.5s per host).
    runProbe();
  };

  const pickCandidate = (url: string) => {
    setOpen(false);
    clearCandidates();
    // applyGatewayUrl also flips relay → remote for http URLs — the
    // same entry point SplashScreen's chooser uses.
    applyGatewayUrl(url);
    void checkHealth();
  };

  const label = isLocal
    ? isStarting
      ? t("splashScreen.bannerLocalStarting")
      : t("splashScreen.bannerLocalDown")
    : t("splashScreen.bannerRemoteDown");

  return (
    <div
      ref={wrapRef}
      className="relative"
      // The title bar is a native drag region; without this the mousedown
      // that opens the popover also starts a window drag.
      onMouseDown={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="inline-flex h-6 shrink-0 items-center gap-1.5 rounded-full bg-amber-500 pr-2 pl-2.5 text-xs font-medium text-white transition-colors hover:bg-amber-600 dark:bg-amber-600 dark:hover:bg-amber-500"
      >
        {/* Breathing = the outage is live and unattended. Pinned to the
            icon, not the whole chip: pulsing the label would flicker the
            text and read as a rendering bug. */}
        <Wifi className="h-3.5 w-3.5 shrink-0 animate-pulse" />
        <span className="shrink-0">{label}</span>
        {candidates.length > 0 && (
          <span className="rounded-full bg-white/25 px-1.5 text-11 tabular-nums">
            {candidates.length}
          </span>
        )}
        <ChevronDown
          className={cn("h-3 w-3 shrink-0 opacity-80 transition-transform", open && "rotate-180")}
        />
      </button>

      {open && (
        <div className="absolute top-full left-0 z-30 mt-1 w-80 rounded-lg border border-border-outer bg-page-bg p-2 shadow-lg">
          <p className="px-1 pb-1.5 text-xs font-medium text-text-secondary">{label}</p>

          {candidates.length > 0 && (
            <>
              <p className="px-1 pb-1 text-11 text-text-tertiary">
                {t("splashScreen.candidateDetected")}
              </p>
              {/* Vertical list, not a horizontal pill strip: a long URL
                  history would otherwise overflow into an unreadable
                  single-line scroller. */}
              <ul className="mb-1.5 flex max-h-56 flex-col gap-1 overflow-auto">
                {candidates.map((c) => (
                  <li key={c.url}>
                    <button
                      type="button"
                      onClick={() => pickCandidate(c.url)}
                      title={c.url}
                      className="btn-accent flex w-full items-center justify-between rounded px-2.5 py-1.5 text-left"
                    >
                      {/* White on the accent fill — never the theme text
                          tokens, which are unreadable on a solid accent. */}
                      <span className="truncate font-mono text-xs" title={c.url}>
                        {c.url}
                      </span>
                      <span className="ml-2 shrink-0 text-11 text-white/80">
                        {Math.round(c.latencyMs)}ms
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}

          <div className="flex justify-end gap-1.5 border-t border-border-divider pt-1.5">
            {isLocal && !isStarting && (
              <button
                type="button"
                onClick={startLocalGateway}
                className="btn-solid rounded-md px-2 py-1 text-xs font-medium"
              >
                {t("splashScreen.bannerStart")}
              </button>
            )}
            <button
              type="button"
              onClick={onRetry}
              className="btn-solid rounded-md px-2 py-1 text-xs font-medium"
            >
              {t("splashScreen.bannerRetry")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
