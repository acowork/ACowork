/**
 * Global UI style constants for consistent component appearance.
 * All input fields, buttons, and interactive elements should use these tokens.
 */

// ── Input field styles ──────────────────────────────────────────────

/** Standard input field (text, number, etc.) */
export const inputBase =
  "w-full rounded border border-input-border bg-input-bg px-3 py-[var(--ui-input-py)] text-xs outline-none transition-colors focus:border-[var(--color-accent)] ";

/** Read-only input field */
export const inputReadonly =
  "rounded border border-zinc-200 bg-zinc-50 px-3 py-[var(--ui-input-py)] text-xs dark:border-zinc-700 dark:bg-zinc-800 ";

// Note: dropdown styling was historically provided via `selectBase` and
// `selectArrowStyle` here. Both have been consolidated into the
// `Dropdown` component at components/common/Dropdown.tsx, which owns the
// SVG arrow + appearance-none contract. All call sites have been
// migrated; do not reintroduce these tokens.

/** Font-mono input (for API keys, codes) */
export const inputMono =
  "rounded border border-zinc-200 px-3 py-[var(--ui-input-py)] font-mono text-xs dark:border-zinc-700 dark:bg-zinc-900 ";

// ── Button styles ───────────────────────────────────────────────────

/**
 * Toolbar button (borderless, compact) — used for Model/Workspace selectors.
 *
 * The `disabled:` variants are load-bearing, not decoration: ADR-076 read-only
 * sessions gate the whole session-write toolbar on the `disabled` attribute, and
 * a bare `disabled` with no visual difference is indistinguishable from an
 * enabled button — users click and nothing happens. `hover:bg-transparent` is
 * the key part: without it a disabled button still lights up on hover while
 * refusing the click, which reads as a broken control.
 */
export const toolbarButton =
  "inline-flex items-center gap-1 rounded px-2 py-1.5 text-xs transition-colors text-text-tertiary hover:bg-zinc-200 dark:hover:bg-zinc-700 hover:text-zinc-700 dark:hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent dark:disabled:hover:bg-transparent";

/**
 * Composer right-cluster icon button — visibility / context-usage /
 * attachment / send.
 *
 * Fixed 28x28 (`h-7 w-7`) + `rounded-md` so all four hover highlight
 * rectangles are identical. Do NOT reach for `toolbarButton` here: it is
 * sized for a text label (`px-2 py-1.5` breathing room around the label),
 * which renders 30px wide with a 4px radius — 2px wider and visibly
 * boxier next to these. Icon-only buttons share this token instead.
 *
 * `disabled:*` carries the same load as `toolbarButton`: a disabled send
 * button with no visual difference is indistinguishable from an enabled
 * one, and `hover:bg-transparent` keeps a disabled button from lighting
 * up while refusing the click.
 */
export const toolbarIconButton =
  "inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-md transition-colors text-text-tertiary hover:bg-zinc-200 dark:hover:bg-zinc-700 hover:text-zinc-700 dark:hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent dark:disabled:hover:bg-transparent";

/** Toolbar button active state */
export const toolbarButtonActive =
  "bg-zinc-200 dark:bg-zinc-700 text-text ";

/** Dialog action button (Cancel/Save) — fixed width */
export const dialogButton =
  "w-20 rounded px-3 py-[var(--ui-btn-py)] text-xs font-medium text-center";

/** Dialog primary action (Save) */
export const dialogButtonPrimary =
  "w-20 rounded bg-zinc-800 px-3 py-[var(--ui-btn-py)] text-xs font-medium text-center text-white hover:bg-zinc-700 disabled:opacity-50 dark:bg-zinc-700 dark:hover:bg-zinc-600";

/** Dialog secondary action (Cancel) */
export const dialogButtonSecondary =
  "w-20 rounded px-3 py-[var(--ui-btn-py)] text-xs font-medium text-center text-text-secondary hover:bg-zinc-100  dark:hover:bg-zinc-700";

// ── Test result styles ──────────────────────────────────────────────

/** Test result message (success/error) */
export const testResultBase =
  "rounded-md px-3 py-[var(--ui-btn-py)] text-xs truncate";

// ── Chat banner slot ──────────────────────────────────────────────

/**
 * Wrapper class for inline banners rendered inside the chat messages scroll
 * container (DebugPausedBanner, RetryWaitBanner, …). The banner component is
 * expected to render THIS wrapper itself and `return null` when not visible,
 * so that no DOM element — and crucially, no `mt-1.5` margin — is left behind
 * when the banner is hidden. An always-rendered empty wrapper with `mt-1.5`
 * would silently push sibling content below the scroll viewport and trigger
 * a phantom scrollbar on empty sessions.
 */
export const bannerSlot =
  "mt-1.5 flex justify-center px-6";

export const testResultSuccess =
  "bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400";

export const testResultError =
  "bg-red-50 text-red-700 dark:bg-red-900/20 dark:text-red-400";
