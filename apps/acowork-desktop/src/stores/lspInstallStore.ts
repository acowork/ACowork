//! LSP install progress, keyed by canonical language.
//!
//! Why this is a store and not component state: an install is one long
//! HTTP request (`POST /api/lsp/install/{language}` — apt/rustup/npm
//! downloads run for minutes), but `LspTab` is unmounted every time the
//! user leaves the Harness view (AppLayout renders views conditionally).
//! Local `useState` lost the running install on the first view switch:
//! the row came back as "not installed" while the script was still going,
//! with no spinner and no result — and a second click fired a second
//! install over the first.
//!
//! Holding the progress here makes it survive the unmount. Same reason, and
//! same shape, as the MCP install progress in `useMcpStore` (ADR-072).

import { create } from "zustand";

/** Terminal outcome of one install attempt. */
export interface LspInstallOutcome {
  success: boolean;
  stdout: string;
  stderr: string;
}

interface LspInstallState {
  /** Languages with an install in flight (row renders the busy state). */
  installing: string[];
  /** Terminal outcome of the most recent attempt per language. */
  outcomes: Record<string, LspInstallOutcome | undefined>;
  /** Record an install as started (called before the request fires). */
  beginInstall: (language: string) => void;
  /** Record a terminal outcome and clear the busy state. */
  endInstall: (language: string, outcome: LspInstallOutcome) => void;
}

export const useLspInstallStore = create<LspInstallState>((set) => ({
  installing: [],
  outcomes: {},

  beginInstall: (language) =>
    set((s) => ({
      installing: s.installing.includes(language) ? s.installing : [...s.installing, language],
      outcomes: { ...s.outcomes, [language]: undefined },
    })),

  endInstall: (language, outcome) =>
    set((s) => ({
      installing: s.installing.filter((l) => l !== language),
      outcomes: { ...s.outcomes, [language]: outcome },
    })),
}));