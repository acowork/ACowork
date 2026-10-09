//! Self-check for ProviderLogo — the smallest thing that fails if the
//! offline table, the model→brand matching, or the provider fallback drifts.
//!
//! Guards the four behaviours the model picker depends on:
//!   1. MODEL NAME WINS: the button shows a model, so `claude-*` must show
//!      Anthropic and `gemini-*` Google — even when the provider id says
//!      something else entirely (custom / aggregate gateway).
//!   2. provider fallback: a model name with no known prefix falls back to
//!      the provider's own mark, and only then to the remote chain.
//!   3. vendor overrides + compound-id splitting (mistral→mistralai,
//!      google-vertex→googlecloud, "minimax token plan"→minimax).
//!   4. remote chain: unbundled provider still tries Simple Icons, then
//!      models.dev + `dark:invert`, then hides itself.
//!
//! Slugs are asserted through `resolveBundledSlug` rather than the rendered
//! <img> src: Vite hashes the asset path and inlines small SVGs as data URIs,
//! so the src string isn't stable across build configs.

import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { ProviderLogo, resolveBundledSlug } from "./ProviderLogo";
import { getGatewayUrl } from "../../lib/config";

// vitest serves modules over http, so import.meta.url isn't a file:// URL —
// anchor asset scans at the vite root instead.
const LOGO_DIR = path.resolve(process.cwd(), "src/assets/provider-logos");

/** Filenames of bundled assets failing `check`, for asset-quality assertions. */
function bundledAsset(check: "stroke-only" | "duplicate"): string[] {
  const assets = fs
    .readdirSync(LOGO_DIR)
    .filter((f) => f.endsWith(".svg"))
    .map((f) => ({ name: f, body: fs.readFileSync(path.join(LOGO_DIR, f), "utf8") }));
  if (check === "stroke-only") return assets.filter((a) => /stroke\s*=/.test(a.body)).map((a) => a.name);
  // Two vendors sharing a byte-identical file means one is a stand-in.
  const byHash = new Map<string, string[]>();
  for (const a of assets) {
    const hash = createHash("sha256").update(a.body).digest("hex");
    byHash.set(hash, [...(byHash.get(hash) ?? []), a.name]);
  }
  return [...byHash.values()].filter((g) => g.length > 1).flat();
}

describe("resolveBundledSlug — model name decides", () => {
  it("maps a model family to its maker, ignoring the provider id", () => {
    const cases: Array<[string, string]> = [
      ["claude-sonnet-4.5", "anthropic"],
      ["claude-opus-4-1", "anthropic"],
      ["gemini-2.5-pro", "google"],
      ["gemma-3-27b", "google"],
      ["gpt-4o-mini", "openai"],
      ["o3-pro", "openai"],
      ["deepseek-r1", "deepseek"],
      ["glm-4.6", "zhipuai"],
      ["chatglm-4", "zhipuai"],
      ["kimi-k2", "kimi"],
      // Legacy Moonshot line: no "kimi" in the name, but Kimi is the mark
      // users know the vendor by — same collapse as glm→zhipuai.
      ["moonshot-v1-8k", "kimi"],
      ["moonshot-v1-32k", "kimi"],
      ["minimax-m2", "minimax"],
      ["qwen3-max", "qwen"],
      ["qwq-32b", "qwen"],
      ["mimo-vl-7b", "xiaomi"],
      ["hunyuan-turbos", "tencenthy"],
      // Seed / Doubao have no mark of their own — they borrow ByteDance's.
      ["seed-1.6-flash", "bytedance"],
      ["doubao-1-5-pro", "bytedance"],
      ["mistral-large-3", "mistralai"],
      ["mixtral-8x7b", "mistralai"],
      ["llama-4-70b", "meta"],
      ["grok-4", "xai"],
      ["sonar-pro", "perplexity"],
    ];
    for (const [modelId, slug] of cases) {
      // Provider id deliberately unrelated: the model name must win.
      expect(resolveBundledSlug("acme-gateway", modelId), modelId).toBe(slug);
    }
  });

  it("resolves through an aggregate gateway's `vendor/model` prefix", () => {
    const cases: Array<[string, string]> = [
      ["deepseek/deepseek-v3.1", "deepseek"],
      ["moonshotai/kimi-k2", "kimi"],
      ["moonshotai/moonshot-v1-8k", "kimi"],
      ["minimax/minimax-m2", "minimax"],
    ];
    for (const [modelId, slug] of cases) {
      expect(resolveBundledSlug("openrouter", modelId), modelId).toBe(slug);
    }
  });

  it("prefers the model over the provider even when both are known", () => {
    // A gateway serving Claude models: the model decides, not the gateway.
    expect(resolveBundledSlug("openai", "claude-sonnet-4.5")).toBe("anthropic");
    expect(resolveBundledSlug("anthropic", "gpt-4o")).toBe("openai");
  });

  it("matches case-insensitively", () => {
    expect(resolveBundledSlug("x", "Claude-Sonnet-4.5")).toBe("anthropic");
    expect(resolveBundledSlug("x", "GPT-4o")).toBe("openai");
  });
});

describe("resolveBundledSlug — provider fallback", () => {
  it("uses the provider mark when the model name matches nothing", () => {
    const cases: Array<[string, string, string]> = [
      ["anthropic", "acme-internal-v2", "anthropic"],
      ["google", "acme-internal-v2", "google"],
      ["mistral", "acme-internal-v2", "mistralai"],
      ["alibaba-cn", "acme-internal-v2", "alibabacloud"],
      ["google-vertex", "acme-internal-v2", "googlecloud"],
      ["minimax token plan", "acme-internal-v2", "minimax"],
      ["zhipuai-coding-plan", "acme-internal-v2", "zhipuai"],
      // The provider section header resolves with no modelId and has to name
      // the company — so moonshotai keeps its own mark even though every one
      // of its models shows the Kimi mark.
      ["moonshotai", "acme-internal-v2", "moonshotai"],
      ["moonshotai-cn", "acme-internal-v2", "moonshotai"],
    ];
    for (const [providerId, modelId, slug] of cases) {
      expect(resolveBundledSlug(providerId, modelId), providerId).toBe(slug);
    }
  });

  it("returns null when neither the model nor the provider is known offline", () => {
    expect(resolveBundledSlug("acme-internal-gw", "acme-secret-v2")).toBeNull();
    expect(resolveBundledSlug("acme-internal-gw")).toBeNull();
  });

  it("resolves a user-defined provider offline, with no network fetch", () => {
    // Regression: `AddProviderFlow` slugifies a custom provider's name into
    // `custom-<slug>`, so every custom gateway used to miss the bundled table,
    // 404 on Simple Icons, and only then pull the generic sparkle from
    // models.dev — two round-trips re-run on every model-menu open.
    // The model name still wins when it names a known vendor (a custom proxy
    // serving `gpt-4o` shows OpenAI, by design); the generic mark is what
    // covers everything else — the provider section header, and models the
    // user named themselves.
    for (const id of ["custom-my-gpt-proxy", "custom-acme", "custom-weird_name-2"]) {
      expect(resolveBundledSlug(id, "acme-secret-v2"), id).toBe("custom");
      expect(resolveBundledSlug(id), id).toBe("custom");
    }
    expect(resolveBundledSlug("custom-my-gpt-proxy", "gpt-4o")).toBe("openai");

    const { container } = render(<ProviderLogo providerId="custom-my-gpt-proxy" />);
    const src = container.querySelector("img")?.getAttribute("src") ?? "";
    expect(src).toBeTruthy();
    expect(src.startsWith("http")).toBe(false);
    // currentColor inside an <img> resolves to black → needs the dark invert.
    expect(container.querySelector("img")?.className).toContain("dark:invert");
  });
});

describe("ProviderLogo rendering", () => {
  it("serves a bundled mark without touching the network", () => {
    const { container } = render(<ProviderLogo providerId="anthropic" modelId="claude-sonnet-4.5" />);
    const src = container.querySelector("img")?.getAttribute("src") ?? "";
    expect(src).toBeTruthy();
    expect(src.startsWith("http")).toBe(false);
  });

  it("inverts monochrome bundled marks for dark mode", () => {
    // models.dev sources use fill="currentColor" → black inside an <img>,
    // which is invisible on a dark toolbar.
    for (const id of ["openai", "zhipuai", "kimi", "ollama"]) {
      const { container } = render(<ProviderLogo providerId={id} />);
      expect(container.querySelector("img")?.className, id).toContain("dark:invert");
    }
    // Brand-colored marks must NOT be inverted (their tint is the brand).
    const { container } = render(<ProviderLogo providerId="anthropic" />);
    expect(container.querySelector("img")?.className).not.toContain("dark:invert");
  });

  it("ships no stroke-only asset (invisible at 12px)", () => {
    // Regression: models.dev serves ONE generic sparkle placeholder for every
    // provider it lacks a real mark for. Dropping that file in under a vendor
    // slug (glm, ollama, seed, doubao) made the picker button look broken —
    // `glm-*` models resolved fine but drew nothing. A stroke-based mark at
    // 12px is ~0.75px of currentColor, so: no bundled asset may be stroke-only.
    expect(bundledAsset("stroke-only")).toEqual([]);
  });

  it("ships no duplicated asset (the placeholder fingerprint)", () => {
    // The placeholder answers 200 for ANY unknown provider id, so "the
    // download succeeded" proves nothing — byte-identical files do. models.dev
    // served the same 1421-byte sparkle for seed, doubao and seedance.
    expect(bundledAsset("duplicate")).toEqual([]);
  });

  it("falls back to the remote chain when nothing is bundled", () => {
    // Both tiers go through the Gateway so the browser can cache them:
    // models.dev and Simple Icons both send `max-age=0, must-revalidate`,
    // which re-fetches every icon on every menu open.
    const gw = getGatewayUrl();
    const { container } = render(<ProviderLogo providerId="acme-internal-gw" />);
    const img = container.querySelector("img") as HTMLImageElement;
    // Tier 1: Simple Icons, brand-tinted → no invert.
    expect(img.getAttribute("src")).toBe(`${gw}/api/providers/acme/logo?src=si`);

    fireEvent.error(img); // Simple Icons 404 → models.dev
    // Tier 2: models.dev, currentColor (black) → needs the dark-mode invert.
    expect(img.getAttribute("src")).toBe(`${gw}/api/providers/acme-internal-gw/logo`);
    expect(img.className).toContain("dark:invert");
  });

  it("never points an <img> straight at an upstream CDN", () => {
    // The whole point of the Gateway hop: a direct models.dev / Simple Icons
    // URL re-validates on every open (max-age=0), so the 180-row provider
    // list re-fetches icon by icon and visibly fills in.
    const { container } = render(<ProviderLogo providerId="some-unknown-gw" />);
    const img = container.querySelector("img") as HTMLImageElement;
    fireEvent.error(img);
    const srcs = [img.getAttribute("src"), (img as HTMLImageElement).src];
    for (const src of srcs) {
      expect(src ?? "").not.toMatch(/models\.dev|cdn\.simpleicons\.org/);
      expect(src ?? "").toContain("/api/providers/");
    }
  });

  it("percent-encodes the provider id before it reaches the URL", () => {
    // `{provider}` is spliced into a remote URL on the Gateway side; a raw
    // id with a slash or space would reshape the path it matches. Tier 1 uses
    // the slug (first segment), tier 2 the raw id — both must be encoded.
    const { container } = render(<ProviderLogo providerId="weird id/../x" />);
    const img = container.querySelector("img") as HTMLImageElement;
    expect(img.getAttribute("src")).toContain("weird/logo?src=si");
    fireEvent.error(img);
    expect(img.getAttribute("src")).toContain("weird%20id%2F..%2Fx/logo");
  });

  it("hides itself when both remote sources 404", () => {
    const { container } = render(<ProviderLogo providerId="acme-internal-gw" />);
    const img = container.querySelector("img") as HTMLImageElement;
    fireEvent.error(img); // tier 1 fails → tier 2
    fireEvent.error(img); // tier 2 fails → hide
    expect(img.style.display).toBe("none");
  });
});
