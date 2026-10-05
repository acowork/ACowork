import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/** The `--text-9` / `--text-10` / `--text-11` micro steps (globals.css
 *  @theme). tailwind-merge only knows Tailwind's own t-shirt scale, so it
 *  files a bare `text-10` under `text-color` — and any real colour class
 *  merged in the same call then DELETES the font size:
 *  `cn("text-10", "text-text-tertiary")` came out as `text-text-tertiary`.
 *  With no font-size left, the element falls back to the inherited body
 *  size (14px), which is why the memory-row badges and status read
 *  *larger* than an 11px card title. Registering the numeric steps as
 *  font sizes fixes every `cn()` caller at once, including the shared
 *  Badge (whose `text-9` never survived a tone colour before). */
const isTextStep = (value: string) => /^\d+$/.test(value);

const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": [{ text: [isTextStep] }],
    },
  },
});

/** Merge Tailwind CSS classes with clsx */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Format a 0-100 percentage value as an integer string (no decimals).
 *
 * Shared by the context-usage and cache-hit-rate displays so the two
 * percentages always render with the same number of decimal places (0)
 * and stay visually aligned when shown side by side (popover, session
 * status block, bottom status bar).  `usage_percent` is already an
 * integer from the Runtime; cache-hit ratios are floored to match.
 *
 * Truncation, not rounding.  We deliberately use `Math.floor` rather
 * than `Math.round` so a sub-100% usage (e.g. `44.6K / 44.8K =
 * 99.55%`) never displays as `100%`.  Showing `100%` when the
 * numbers above the bar are visibly less than full would read as
 * either a calculation bug or a faked "full" state, neither of which
 * we want next to the live token counts.  The cost is at most one
 * percentage point of under-reporting; the benefit is that the bar
 * never lies about its numerator.
 */
export function formatPercent(n: number): string {
  if (!Number.isFinite(n)) return "0";
  return String(Math.floor(n));
}
