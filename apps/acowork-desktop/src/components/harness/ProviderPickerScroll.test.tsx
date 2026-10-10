/**
 * Provider groups SCROLL — they must not grow, and they must not paginate.
 *
 * Why this file exists:
 *   The remote group used to render `slice(0, 5)` plus a "Show all (N)"
 *   button. Clicking it expanded the group to the full catalog (90+ providers
 *   ≈ 4000px tall), which blew the surrounding container open — in the
 *   onboarding wizard the card simply ran off screen — and search hits landed
 *   under yet another rule (uncapped). Both behaviours are replaced by a
 *   three-row scroll region, so every provider is reachable with the wheel and
 *   the layout height is fixed.
 *
 *   The cap is a `calc()` against `--ui-font-size`, not a px number: the rows
 *   are text-sized (12px label + 10px model count), so a frozen px height
 *   would show 2 rows at XXL and 4 at S. jsdom resolves no cascade, so the
 *   expression itself is the spec here.
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import i18n from "../../i18n";
import { ProviderPicker } from "./ProviderPicker";
import type { ProviderListEntry } from "../../lib/types";

const remote = Array.from({ length: 12 }, (_, i) => ({
  id: `remote-${i}`,
  name: `Remote ${i}`,
  model_count: 5,
}));
const providers: ProviderListEntry[] = [
  ...remote,
  { id: "ollama", name: "Ollama", model_count: 0, local: true },
];

function renderPicker() {
  return render(
    <ProviderPicker
      providers={providers}
      keys={[]}
      onConnect={vi.fn()}
      onAddCustom={vi.fn()}
    />,
  );
}

describe("ProviderPicker group scrolling", () => {
  it("renders the whole catalog instead of slicing it", () => {
    const { container } = renderPicker();
    remote.forEach((p) => expect(screen.getByText(p.name)).toBeTruthy());
    // The "Show all" affordance is gone with the slice — nothing may reappear.
    expect(
      Array.from(container.querySelectorAll("button")).filter((b) =>
        /Show all/i.test(b.textContent ?? ""),
      ),
      "no pagination button",
    ).toHaveLength(0);
  });

  it("caps every group body with a three-row height tied to --ui-font-size", () => {
    const { container } = renderPicker();
    const capped = Array.from(
      container.querySelectorAll<HTMLElement>("[style*='max-height']"),
    );
    // Remote + Local + Custom groups.
    expect(capped, "every group body must be capped").toHaveLength(3);
    capped.forEach((el) => {
      expect(el.style.maxHeight, "cap must track the global font step").toContain(
        "calc(var(--ui-font-size",
      );
      expect(el.style.overflowY, "capped body must scroll").toBe("auto");
    });
    // Sanity on the arithmetic: 3 two-line rows ≈ 10.2 × the 14px base.
    expect(capped[0].style.maxHeight).toMatch(/\*\s*10\.2\)/);
  });

  it("keeps search hits inside the same cap", () => {
    const { container } = renderPicker();
    const search = screen.getByPlaceholderText(i18n.t("harness.searchProviders"));
    // "remote-1" matches Remote 1 / 10 / 11 — more than fits, and the hits
    // used to be uncapped while the unfiltered view was cut at 5.
    fireEvent.change(search, { target: { value: "remote-1" } });
    expect(screen.getByText("Remote 10")).toBeTruthy();
    const body = screen
      .getByText("Remote 10")
      .closest<HTMLElement>("[style*='max-height']");
    expect(body, "filtered rows must live inside a capped body").toBeTruthy();
    expect(body!.style.overflowY, "and stay scrollable").toBe("auto");
    expect(
      container.querySelectorAll("[style*='max-height']").length,
      "the cap is one expression, not a per-branch duplicate",
    ).toBe(3);
  });
});
