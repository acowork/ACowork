/**
 * Self-check for the toolbar trigger label font size.
 *
 * User-visible feature: the model / workspace / skill / reasoning-effort
 * labels in the chat input toolbar follow the global font-size setting
 * (settingsStore `--ui-font-size`, Ctrl+= / Ctrl+-). They previously hard
 * coded `0.75rem`, so those four buttons stayed at 12px while the rest of
 * the app scaled — the one place in the toolbar that ignored the setting.
 *
 * jsdom computes no layout and never resolves calc(), so this reads the
 * source instead of the rendered DOM (same approach as capsule.test.tsx).
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";

const src = readFileSync(path.join(__dirname, "ToolbarDropdown.tsx"), "utf8");

describe("ToolbarDropdownTrigger label font size", () => {
  it("tracks the global --ui-font-size instead of a fixed rem", () => {
    expect(src).toContain("calc(var(--ui-font-size, 0.875rem)");
    // No bare rem length anywhere near the label span.
    expect(src).not.toMatch(/fontSize:\s*"[\d.]+rem"/);
  });

  it("applies it to the [data-toolbar-text] label span", () => {
    const span = src.slice(src.indexOf("data-toolbar-text"));
    expect(span).toMatch(/style=\{\{ fontSize: LABEL_FONT_SIZE/);
  });
});