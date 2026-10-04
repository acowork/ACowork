/**
 * The global font-size knob must scale the WHOLE app, not just chat.
 *
 * Why this file exists:
 *   Settings → Appearance → font size writes `--ui-font-size` onto
 *   :root. Until recently it only reached chat, because chat bound the
 *   variable to ~50 elements via inline `style={{fontSize: ...}}` while
 *   every other view (pm / doc / extension / harness / settings / setup)
 *   used fixed `rem` Tailwind sizes. Changing the setting resized chat
 *   and nothing else, so font sizes visibly diverged per view.
 *
 * What this pins (the two things that silently break the unification):
 *   1. `body` font-size tracks `--ui-font-size` — the inheritance root
 *      every un-classed row (the pm project list among them) reads.
 *   2. The `--text-*` tokens are `em` ratios of that same variable, not
 *      fixed `rem`. A `rem` here would make the setting a no-op outside
 *      chat again — and, worse, would also scale every layout offset
 *      that is expressed in rem.
 *
 * Why source scanning:
 *   These are CSS custom properties consumed by Tailwind's generated
 *   utilities. jsdom resolves no cascade and computes no font sizes, so
 *   the declaration text IS the spec — same approach as
 *   capsule.test.tsx.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(
  resolve(__dirname, "..", "styles", "globals.css"),
  "utf8",
);

describe("global font size scales the whole app", () => {
  it("declares --ui-font-size on :root as the pre-JS default", () => {
    // settingsStore overwrites it at boot; without a CSS default the
    // first paint would resolve var(--ui-font-size) to nothing.
    const root = css.match(/:root \{[^}]*--ui-font-size:\s*([^;]+);/);
    expect(root, ":root has no --ui-font-size").not.toBeNull();
    // Must equal DEFAULT_FONT_SIZE in lib/defaults.ts (0.875) — a
    // mismatch makes the app jump on first paint after a reload.
    expect(root![1].trim()).toBe("0.875rem");
  });

  it("binds body font-size to --ui-font-size, not a fixed size", () => {
    const body = css.match(/^body \{([\s\S]*?)\n\}/m);
    expect(body, "no body rule").not.toBeNull();
    expect(body![1]).toContain("font-size: var(--ui-font-size");
    // The old value pinned it to a static ratio, which is exactly the
    // state this file exists to prevent coming back.
    expect(body![1]).not.toContain("font-size: var(--text-base)");
  });

  it("expresses every --text-* token as an em ratio", () => {
    const tokens = ["xs", "sm", "base", "lg", "xl"];
    for (const t of tokens) {
      const m = css.match(new RegExp(`--text-${t}:\\s*([^;]+);`));
      expect(m, `--text-${t} is not declared`).not.toBeNull();
      // `em` keeps the 12/14/16/18/20 ratio intact under any base size.
      // `rem` would freeze the scale AND drag every rem-based offset
      // with it, so the failure mode is layout drift, not just no-op.
      expect(m![1].trim(), `--text-${t} must be em, not rem`).toMatch(/em$/);
    }
  });

  it("leaves the NavBar alone (icon-only, fixed px sizing)", () => {
    // The nav buttons carry no text and size their icons in fixed units
    // (`h-6 w-6`), so they do not move when the font size changes. Pin
    // the icon sizing so a future "let's make it em too" edit does not
    // silently start resizing the rail.
    const nav = readFileSync(
      resolve(__dirname, "..", "components", "layout", "NavBar.tsx"),
      "utf8",
    );
    expect(nav).toContain("h-6 w-6");
    expect(nav).not.toMatch(/className="[^"]*text-(xs|sm|base|lg|xl)[^"]*"/);
  });
});
