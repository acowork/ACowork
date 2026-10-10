import { useEffect, useRef, useState } from "react";
import { Globe } from "lucide-react";
import { useTranslation } from "../../i18n/useTranslation";
import { useGatewayStore } from "../../stores/gatewayStore";
import { useSettingsStore } from "../../stores/settingsStore";
import { probeGateways, type ProbeResult } from "../../lib/gateway-probe";
import { listRowHover, listRowSelected } from "../../lib/ui-styles";
import { cn } from "../../lib/utils";
import { Tooltip } from "../common/Tooltip";
import { Dropdown } from "../common/Dropdown";
import type { GatewayMode } from "../../lib/types";

/** Probe verdicts rendered per row. `pending` is the pre-settle state so
 *  a row never renders as "offline" while its probe is still in flight —
 *  a black-holed address would otherwise flash red for 1.5s then correct. */
type Verdict = "pending" | "ok" | "fail";

/**
 * Title-bar gateway switcher — a minimized Settings ▸ Gateway.
 *
 * Answers the two questions the title bar was missing:
 *   1. WHICH gateway am I on right now? (the URL is otherwise only
 *      visible after opening Settings)
 *   2. what else is alive on this network? (the outage chip only
 *      appears once the current Gateway is already unreachable)
 *
 * Reuses the pieces instead of re-deriving them:
 *   - the list is `settingsStore.gatewayUrlHistory` — the LRU of URLs
 *     that ACTUALLY connected (SplashScreen/AppLayout record on the
 *     connection lifecycle, never on a keystroke), so a typo or a dead
 *     host can never show up here as a candidate.
 *   - probing is `probeGateways` (per-host AbortController, 1.5s budget,
 *     `onSettled` per host) driven through local state. Deliberately NOT
 *     `probeKnownCandidates()`: that one is the outage failover path —
 *     it returns 0 while connected, returns 0 in local mode, and writes
 *     into `gatewayStore.candidates` where the outage chip owns the
 *     rendering. This menu asks the same question in the opposite
 *     situation (connected, browsing) so it must not share that state.
 *   - switching is `applyGatewayUrl` + `checkHealth`, the exact pair
 *     `GatewayStatusChip.pickCandidate` uses — so the relay→remote
 *     http:// correction and the authStore re-validation happen here too.
 *   - the trigger glyph is `Globe`, the SAME icon SettingsPage gives its
 *     gateway tab (SettingsPage.tsx:38). The title-bar button and that
 *     tab are two doors to one feature, so they must not learn two
 *     shapes — a user who finds `Globe` in Settings must meet the same
 *     mark in the title bar. `Server` was tried first and is wrong:
 *     it reads as "a machine running a service", not "which network am I
 *     on", and it was the only lucide glyph this app used exactly once.
 *   - row tints come from `listRowSelected` / `listRowHover`
 *     (lib/ui-styles.ts) — the app-wide accent-tint family every other
 *     list (popups, side panels, file tree, search results) uses, so the
 *     cursor passing over a row reads the same here as everywhere else.
 *
 * The mode control is a label + `<select>` listbox, not three radios.
 * Two reasons, both measured against the real widget:
 *   - the three labels ("Local" / "Remote (LAN)" / "Relay (WAN)")
 *     wrapped onto two rows inside the popover, which reads as a
 *     broken layout rather than a compact mode picker;
 *   - a native listbox already has the keyboard and screen-reader
 *     contract (`role=listbox`, arrow keys, type-ahead) a hand-rolled
 *     popover list would have to re-implement. The shared `Dropdown` is
 *     the app's one `<select>` wrapper — the same one the settings tabs
 *     use, so the control looks identical everywhere.
 *
 * The mode listbox is a FILTER, not a config editor: picking a row is
 * what commits. `setGatewayMode` pushes to Rust and rebuilds MQTT, and
 * firing that from a stray selection inside a title-bar popover would
 * switch the connection out from under the user with no address chosen.
 * The live configuration's mode still marks the current row.
 */
export function GatewaySwitcherMenu() {
  const { t } = useTranslation();
  const gatewayUrl = useSettingsStore((s) => s.gatewayUrl);
  const gatewayUrlHistory = useSettingsStore((s) => s.gatewayUrlHistory);
  const configuredMode = useSettingsStore((s) => s.gatewayMode);
  const applyGatewayUrl = useSettingsStore((s) => s.applyGatewayUrl);
  const status = useGatewayStore((s) => s.status);
  const localState = useGatewayStore((s) => s.localState);
  const startLocalGateway = useGatewayStore((s) => s.startLocalGateway);
  const checkHealth = useGatewayStore((s) => s.checkHealth);

  const [open, setOpen] = useState(false);
  // Starts on the live mode so the panel opens on the relevant list
  // instead of defaulting to a tab the user never uses.
  const [tab, setTab] = useState<GatewayMode>(configuredMode);
  const [verdicts, setVerdicts] = useState<Record<string, Verdict>>({});
  const wrapRef = useRef<HTMLDivElement>(null);
  // Bumped per open so a close/open cycle re-probes: liveness decays
  // over time and a stale "online" from the last visit is a lie.
  const [probeRun, setProbeRun] = useState(0);

  // Close on outside click / Escape — the popover must never trap pointer
  // events over the app behind it. Same contract as GatewayStatusChip.
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

  // Remote/relay browse the recorded history; local is pinned to one
  // address (there is no second address to discover on the loopback).
  // The current URL leads the list even if history is empty — a
  // first-run install has a URL but no history yet, and showing an empty
  // panel would leave the user with no address to look at.
  const rows =
    tab === "local"
      ? [gatewayUrl]
      : Array.from(new Set([gatewayUrl, ...gatewayUrlHistory]));

  // Probe the browsed list. `onSettled` folds each host in as it lands
  // so one black-holed address can't hold a reachable one hostage for
  // the full budget.
  useEffect(() => {
    if (!open || tab === "local") return;
    const targets = rows;
    setVerdicts({});
    let cancelled = false;
    void probeGateways(targets, {
      onSettled: (r: ProbeResult) => {
        if (cancelled) return;
        setVerdicts((prev) => ({ ...prev, [r.url]: r.ok ? "ok" : "fail" }));
      },
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, tab, probeRun]);

  const toggle = () => {
    setOpen((v) => {
      if (!v) setProbeRun((n) => n + 1);
      return !v;
    });
  };

  const switchTo = (url: string) => {
    if (url === gatewayUrl) return;
    setOpen(false);
    // Same entry point as the outage chip's candidate pick: persists the
    // URL, pushes it to Rust (rebuilds MQTT), persists the mode, and
    // re-validates the auth token against the new Gateway.
    applyGatewayUrl(url);
    void checkHealth();
  };

  const isLocalTab = tab === "local";

  return (
    <div
      ref={wrapRef}
      className="relative"
      // The title bar is a native drag region; without this the mousedown
      // that opens the popover also starts a window drag.
      onMouseDown={(e) => e.stopPropagation()}
    >
      <Tooltip content={t("titleBar.gateway")} position="bottom">
        <button
          type="button"
          aria-expanded={open}
          aria-label={t("titleBar.gateway")}
          onClick={toggle}
          className={cn(
            "flex h-8 w-8 items-center justify-center rounded transition-colors",
            open
              ? "bg-zinc-300 text-text-primary dark:bg-zinc-700"
              : "text-text-secondary hover:bg-zinc-300 dark:hover:bg-zinc-700",
          )}
        >
          <Globe className="h-3.5 w-3.5" />
        </button>
      </Tooltip>

      {open && (
        <div className="absolute right-0 z-30 mt-1 w-80 rounded-lg border border-border-outer bg-page-bg p-2 shadow-lg">
          {/* Label left, listbox right — one row at any language width.
              The label is the settings tab's own wording so the two
              screens name the concept identically. */}
          <div className="flex items-center gap-2 border-b border-border-divider px-1 pb-2">
            <span className="shrink-0 text-xs text-text-tertiary">
              {t("settings.gatewayMode")}
            </span>
            <Dropdown
              size="small"
              value={tab}
              onChange={(v) => setTab(v as GatewayMode)}
              options={[
                { value: "local", label: t("settings.local") },
                { value: "remote", label: t("settings.remote") },
                { value: "relay", label: t("settings.relay") },
              ]}
              aria-label={t("settings.gatewayMode")}
              className="ml-auto min-w-0 flex-1"
            />
          </div>

          {/* Local: one address, no discovery. The lifecycle row is the
              useful part (a local Gateway that isn't running is the
              common failure), so it gets Start — the same action
              SettingsPage's local card offers. */}
          {isLocalTab ? (
            <div className="pt-2">
              <p className="truncate px-1 font-mono text-xs" title={gatewayUrl}>
                {gatewayUrl}
              </p>
              <div className="mt-1.5 flex items-center justify-between gap-2 px-1">
                <StatusRow status={status} localState={localState} />
                {localState !== "running" && localState !== "starting" && (
                  <button
                    type="button"
                    onClick={() => void startLocalGateway()}
                    className="btn-solid shrink-0 rounded-md px-2 py-1 text-xs font-medium"
                  >
                    {t("settings.startGateway")}
                  </button>
                )}
              </div>
            </div>
          ) : (
            <ul className="mt-1 flex max-h-64 flex-col gap-0.5 overflow-auto">
              {rows.map((url) => {
                const current = url === gatewayUrl;
                // A row is switchable ONLY once its probe has positively
                // answered `/health` — a whitelist, not a blacklist of
                // the failures. Anything else (still in flight, rejected,
                // never probed) is disabled, because clicking it can only
                // do one thing: swap a working Gateway for a connection
                // that then fails `checkHealth`, stranding the user.
                //
                // Gating on `fail` alone left the exact window an attacker
                // — or a user — reaches for: open the menu, click before
                // the first probe settles. A few hundred ms is all it takes,
                // and "the row greyed out a moment ago" is the whole answer
                // to why it now can't be clicked.
                //
                // No local-mode exemption is needed: that tab renders
                // `rows = [gatewayUrl]`, i.e. the current address alone,
                // which `current` already disables.
                const reachable = verdicts[url] === "ok";
                return (
                  <li key={url}>
                    <button
                      type="button"
                      onClick={() => switchTo(url)}
                      disabled={current || !reachable}
                      title={url}
                      className={cn(
                        "flex w-full items-center justify-between gap-2 rounded px-2 py-1.5 text-left transition-colors",
                        // `listRowSelected` bundles its own `hover:` arm —
                        // without it the hover tint would win over the
                        // selection while the cursor sits on the row
                        // (Tailwind emits hover variants after plain
                        // utilities), and the active row would grey out
                        // under the cursor exactly when you click it.
                        current ? listRowSelected : listRowHover,
                        // A disabled row must not light up under the
                        // cursor while refusing the click (`listRowHover`
                        // is a bare `hover:` utility with no disabled arm).
                        !reachable && "cursor-not-allowed opacity-50",
                      )}
                    >
                      <span className="truncate font-mono text-xs">{url}</span>
                      <RowVerdict
                        verdict={verdicts[url]}
                        current={current}
                        configuredMode={configuredMode}
                      />
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

/** Probe verdict for one row. Green dot for the ONE state that makes a row
 *  clickable: a host that actually answered `/health`. Everything else is
 *  spelled out in words — "testing…" while the probe is still in flight,
 *  "Disconnected" once it is rejected — because both of those rows are
 *  disabled, and a disabled row carrying a bare dot that could also mean
 *  "green but I can't tell" leaves the user hunting for the difference.
 *  The current address shows its mode label instead — the dot's question
 *  is "which OTHERS are reachable". */
function RowVerdict({
  verdict,
  current,
  configuredMode,
}: {
  verdict: Verdict | undefined;
  current: boolean;
  configuredMode: GatewayMode;
}) {
  const { t } = useTranslation();
  if (current) {
    // Relay mode's own label matters here: an https:// relay address and
    // a plain remote address look alike in the list, and picking one
    // silently flips the mode (applyGatewayUrl's relay→remote fix).
    return (
      <span className="flex shrink-0 items-center gap-1.5 text-11 text-text-secondary">
        {t(`settings.${configuredMode}`)}
        <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
      </span>
    );
  }
  if (verdict === "fail") {
    return (
      <span className="shrink-0 text-11 text-text-tertiary">
        {t("settings.disconnected")}
      </span>
    );
  }
  if (!verdict) {
    // Not disabled for a reason the user can see — the row is greyed out
    // because it is still being probed, and a bare dot next to it would
    // look like the "offline" mark the row can't undo yet.
    return (
      <span className="shrink-0 text-11 text-text-tertiary">
        {t("settings.testing")}
      </span>
    );
  }
  return (
    <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-500" />
  );
}

/** Connection status line — the same wording as SettingsPage's row so
 *  the two never disagree. localState wins in local mode because that's
 *  where the Gateway actually lives (a remote address can be answered
 *  by an externally-owned process). */
function StatusRow({
  status,
  localState,
}: {
  status: string;
  localState: string;
}) {
  const { t } = useTranslation();
  const local = localState === "running";
  const starting = localState === "starting";
  const tone = local || status === "connected"
    ? "text-emerald-600 dark:text-emerald-400"
    : status === "error"
      ? "text-red-600 dark:text-red-400"
      : "text-text-tertiary";
  const label = starting
    ? t("settings.starting")
    : local || status === "connected"
      ? t("settings.connected")
      : status === "error"
        ? t("settings.error")
        : t("settings.disconnected");
  return (
    <span className={cn("flex items-center gap-2 text-xs", tone)}>
      <span
        className={cn(
          "h-2 w-2 rounded-full",
          local || status === "connected"
            ? "bg-emerald-500"
            : status === "error"
              ? "bg-red-500"
              : "bg-zinc-400",
        )}
      />
      {label}
    </span>
  );
}