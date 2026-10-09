/**
 * Self-check for the composer toolbar dropdowns' selected-row styling.
 *
 * User-visible feature: in the model / reasoning-effort / workspace / skills
 * popups, the CURRENT selection carries a tinted background, not just accent
 * text. Before the fix the selected row was text-only while every *other*
 * row lit up a neutral grey on hover — so the row you had actually chosen
 * read as weaker than the row the cursor happened to be crossing.
 *
 * Why source scanning instead of rendered DOM: the tint is a Tailwind
 * arbitrary value (`bg-[var(--color-accent)]/10`) that jsdom neither
 * resolves nor asserts on. The class strings ARE the spec here — same
 * approach as capsule.test.tsx / ToolbarDropdown.fontSize.test.tsx.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { menuItemHover, menuItemSelected } from "../../lib/ui-styles";

const __dirname = dirname(fileURLToPath(import.meta.url));
const read = (rel: string) => readFileSync(resolve(__dirname, "..", "..", rel), "utf8");

/**
 * Every composer-toolbar popup that renders a "currently selected" row,
 * with the number of selected rows it owns.
 */
const MENUS = [
  ["components/chat/ChatPanel.tsx", 3], // model rows, account fly-out rows, reasoning rows
  ["components/workspace/WorkspaceSelector.tsx", 2], // Agent Home + workspace rows
  ["components/skills/SkillsPanel.tsx", 1],
] as const;

describe("menuItemSelected", () => {
  it("is a background, not just accent text", () => {
    // A text-only selection is the exact bug this pins.
    expect(menuItemSelected).toMatch(/bg-\[var\(--color-accent\)\]/);
    expect(menuItemSelected).toContain("text-[var(--color-accent)]");
    // Keeps reacting to hover, one step stronger than the resting tint.
    expect(menuItemSelected).toContain("hover:bg-[var(--color-accent)]");
    // Unselected rows stay neutral so the tint reads as "this one".
    expect(menuItemHover).toBe("hover:bg-zinc-50 dark:hover:bg-zinc-700/50");
  });
});

it.each(MENUS)("%s routes every selected row through it", (rel, expected) => {
  const src = read(rel);
  // +1 accounts for the import statement itself. Every selected row must
  // pull the token in, so a new menu row cannot ship text-only by default.
  expect(src.split("menuItemSelected").length - 1).toBe(expected + 1);
  // The hand-rolled two-space hover string these rows used before is gone.
  expect(src).not.toContain('"hover:bg-zinc-50  dark:hover:bg-zinc-700/50"');
});
