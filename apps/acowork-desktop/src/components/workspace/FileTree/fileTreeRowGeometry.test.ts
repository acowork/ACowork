/**
 * The file tree's row height is derived in TypeScript, but the row's actual
 * glyph size is declared in CSS. Those two MUST move together:
 * `FileTree.tsx` sizes every virtualizer slot from
 * `fontSize * 16 * TREE_ROW_FONT_RATIO * TREE_ROW_LINE_MULTIPLIER`, and a slot
 * even ~0.4px off the row's natural CSS height leaves an edge where
 * `elementFromPoint(...).closest('[data-rel-path]')` returns null, which
 * silently breaks tree drag & drop (see the notes in FileTree.tsx).
 *
 * jsdom performs no layout, so the guard is textual: read the ratio straight
 * out of globals.css and require the constant to match it.
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  TREE_ROW_FONT_RATIO,
  TREE_ROW_LINE_MULTIPLIER,
} from "./FileTree";

const CSS = readFileSync(resolve(__dirname, "..", "..", "..", "styles", "globals.css"), "utf8");

/** `--ui-text-size: calc(var(--ui-font-size, 0.875rem) * 0.857)` → 0.857 */
function cssTextRatio(): number {
  const m = CSS.match(/--ui-text-size:\s*calc\(\s*var\(--ui-font-size[^)]*\)\s*\*\s*([\d.]+)\s*\)/);
  expect(m, "--ui-text-size must be defined as a ratio of --ui-font-size").not.toBeNull();
  return Number(m![1]);
}

describe("file tree row geometry", () => {
  it("keeps the TS font ratio in sync with --ui-text-size in CSS", () => {
    expect(TREE_ROW_FONT_RATIO).toBe(cssTextRatio());
  });

  it("sizes the row from the shared token, not a one-off utility", () => {
    // `.file-tree-row` is what both FileTreeNode's rows and GitStatusPanel's
    // change rows carry; if it stops using --ui-text-size the two lists drift.
    const rule = CSS.match(/\.file-tree-row\s*\{([^}]*)\}/);
    expect(rule, ".file-tree-row must have a rule in globals.css").not.toBeNull();
    expect(rule![1]).toMatch(/font-size:\s*var\(--ui-text-size\)/);
  });

  it("documents the default-base slot height the multiplier resolves to", () => {
    // 0.875rem @ 16px = 14px base → 12px row text → line-height 1.5 + 2 ×
    // py-[0.2em] = 22.796px. Pinned so an edit to either multiplier is a
    // deliberate change rather than a typo.
    const DEFAULT_BASE_REM = 0.875;
    const rowHeight = DEFAULT_BASE_REM * 16 * TREE_ROW_FONT_RATIO * TREE_ROW_LINE_MULTIPLIER;
    expect(rowHeight).toBeCloseTo(22.796, 3);
  });
});
