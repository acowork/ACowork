/**
 * All four list capsules must share one header height.
 *
 * Why this file exists:
 *   The nav-surface left column appears in pm / doc / extension /
 *   harness / settings, and each originally spelled its own header
 *   band: pm wrapped a 28px search input in `py-2` (≈44px), doc used
 *   `py-1.5` around a 14px title line (≈27px), extensions repeated
 *   pm's, and the harness / settings shell was built from doc's. So the
 *   same pane's header changed height depending on which style it had
 *   copied, and switching between views made the list rows jump.
 *
 * What this pins: every one of those headers binds the same
 * `--ui-list-header-h` token, so the row lists below them start on the
 * same baseline. A `px-N py-N` padding pair here reintroduces exactly
 * the drift this file exists to catch — the token is the point, not the
 * specific pixels.
 *
 * Why source scanning: the token is a CSS custom property; jsdom
 * computes no layout, and these files pull in Tauri / MQTT / monaco
 * stores that make a render assertion unrepresentative. Same approach
 * as capsule.test.tsx and globalFontSize.test.ts.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));
const read = (rel: string) =>
  readFileSync(resolve(__dirname, "..", "..", rel), "utf8");

/** Every nav-surface list column, and what its header band holds. */
const LIST_COLUMNS = [
  ["views/pm/ProjectSidebar.tsx", "search box"],
  ["views/doc/DocTreeSidebar.tsx", "title + action buttons"],
  ["views/ExtensionsView.tsx", "search box"],
  ["components/common/SectionPane.tsx", "section title (harness / settings)"],
] as const;

describe("list column headers share one height", () => {
  it("declares --ui-list-header-h in globals.css", () => {
    const css = read("styles/globals.css");
    const m = css.match(/--ui-list-header-h:\s*([^;]+);/);
    expect(m, "--ui-list-header-h is not declared").not.toBeNull();
    // Equal to --ui-dialog-zone-h (2.75rem / 44px) — the same single-line
    // zone height dialog headers already use, so a list header and a
    // dialog header read as one vertical rhythm.
    expect(m![1].trim()).toBe("2.75rem");
  });

  it.each(LIST_COLUMNS)("%s binds the shared header height (%s)", (rel) => {
    expect(read(rel), `${rel} does not bind --ui-list-header-h`).toContain(
      "min-h-[var(--ui-list-header-h)]",
    );
  });

  it.each(LIST_COLUMNS)("%s no longer hand-rolls a header height (%s)", (rel) => {
    // The header wrapper is the element that carries the token. Scope
    // the check to THAT line only — a file-wide `px-N py-N` sweep also
    // catches dialogs, empty states and buttons that have nothing to do
    // with the header, which is how this assertion first went red on
    // correct code.
    const src = read(rel);
    const headerLine = src
      .split(/\r?\n/)
      .find((l) => l.includes("min-h-[var(--ui-list-header-h)]"));
    expect(headerLine, `${rel} has no header line`).toBeTruthy();
    // `py-N` on the header is what silently overrode the token.
    expect(headerLine!).not.toMatch(/\bpy-\d/);
  });
});
