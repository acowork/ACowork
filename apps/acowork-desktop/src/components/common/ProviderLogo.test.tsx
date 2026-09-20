//! Self-check for ProviderLogo — the smallest thing that fails if the URL
//! chain, alias mapping, or segment-splitting rule drifts. Pure assertion,
//! no DOM mocks.
//!
//! Guards four real-world regressions:
//!   1. tier-1 source + slug format: cdn.simpleicons.org/{slug} is the
//!      contract both call sites (input menu + harness list) depend on.
//!   2. vendor override: mistral→mistralai, alibaba→alibabacloud.
//!   3. first-segment split rule: compound ids like "minimax token plan",
//!      "zhipuai-coding-plan", "minimax_token_plan" collapse to the
//!      parent brand so Simple Icons (parent-brand-only) hits.
//!   4. tier-2 fallback chain: when Simple Icons 404s, the img must swap
//!      to models.dev/logos/{id}.svg and add `dark:invert` so the
//!      monochrome currentColor SVG stays legible in dark mode.

import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { ProviderLogo } from "./ProviderLogo";

describe("ProviderLogo", () => {
  it("starts with the Simple Icons CDN URL", () => {
    const { container } = render(<ProviderLogo providerId="anthropic" />);
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(img?.getAttribute("src")).toBe("https://cdn.simpleicons.org/anthropic");
  });

  it("routes renamed vendors through the alias table", () => {
    const { container } = render(<ProviderLogo providerId="mistral" />);
    expect(container.querySelector("img")?.getAttribute("src"))
      .toBe("https://cdn.simpleicons.org/mistralai");
  });

  it("short-circuits the split rule for compound Google brands (Vertex → googlecloud)", () => {
    // Without the override, `google-vertex` would split to `google` and show
    // the generic G logo. Vertex AI's actual brand mark is the Google Cloud
    // sparkle (Vertex is a Google Cloud service).
    for (const id of ["google-vertex", "google-vertex-anthropic"]) {
      const { container } = render(<ProviderLogo providerId={id} />);
      expect(container.querySelector("img")?.getAttribute("src"))
        .toBe(`https://cdn.simpleicons.org/googlecloud`);
    }
  });

  it("collapses compound ids (space / dash / underscore) to the first segment", () => {
    const cases: Array<[string, string]> = [
      ["minimax token plan", "minimax"],
      ["zhipuai-coding-plan", "zhipuai"],
      ["minimax_token_plan", "minimax"],
    ];
    for (const [providerId, expectedSlug] of cases) {
      const { container } = render(<ProviderLogo providerId={providerId} />);
      expect(container.querySelector("img")?.getAttribute("src"))
        .toBe(`https://cdn.simpleicons.org/${expectedSlug}`);
    }
  });

  it("applies the vendor override after the split (e.g. alibaba-cn → alibabacloud)", () => {
    // -cn variants aren't in the override table any more — the rule is
    // split first, then re-check overrides. Guards that ordering.
    const { container } = render(<ProviderLogo providerId="alibaba-cn" />);
    expect(container.querySelector("img")?.getAttribute("src"))
      .toBe("https://cdn.simpleicons.org/alibabacloud");
  });

  it("falls back to models.dev and adds dark:invert when Simple Icons 404s", () => {
    // jsdom doesn't actually load network images, so img.dispatchEvent('error')
    // is the standard way to simulate a failed load in component tests.
    const { container } = render(<ProviderLogo providerId="openai" />);
    const img = container.querySelector("img") as HTMLImageElement;
    expect(img).not.toBeNull();
    fireEvent.error(img);

    expect(img.getAttribute("src")).toBe("https://models.dev/logos/openai.svg");
    expect(img.className).toContain("dark:invert");
  });

  it("hides itself when both sources 404", () => {
    const { container } = render(<ProviderLogo providerId="openai" />);
    const img = container.querySelector("img") as HTMLImageElement;
    fireEvent.error(img); // tier 1 fails → tier 2
    fireEvent.error(img); // tier 2 fails → hide
    expect(img.style.display).toBe("none");
  });
});