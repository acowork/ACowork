//! ProviderLogo — resolves a brand mark, bundled-first:
//!   1. `src/assets/provider-logos/{slug}.svg`  (offline, the mainstream brands)
//!   2. https://cdn.simpleicons.org/{slug}      (brand-colored, CC0)
//!   3. https://models.dev/logos/{id}.svg       (monochrome currentColor, fallback)
//!
//! Tier 1 also carries `custom.svg` — the generic mark for user-defined
//! providers (`custom-<slug>` ids). models.dev serves that same sparkle for
//! every id it has no brand for, so without it a custom provider cost two
//! network round-trips (a Simple Icons 404, then the logo) on every menu open.
//!
//! If every tier misses the <img> hides itself; callers don't need a fallback.
//!
//! Used by:
//!   - ChatPanel ModelMenu (input box)        — trigger icon + provider section header
//!   - HarnessPage ProviderPicker             — row leading slot

import { cn } from "../../lib/utils";
import { getGatewayUrl } from "../../lib/config";

/**
 * Bundled brand marks, keyed by the same slug the CDN tier uses.
 * `import.meta.glob` keeps this in sync with the files on disk — dropping a
 * new svg in the folder is the whole "add a provider" workflow.
 */
const BUNDLED = import.meta.glob<string>("../../assets/provider-logos/*.svg", {
  eager: true,
  query: "?url",
  import: "default",
});

/** slug → bundled asset URL. Built once at module load (~27 entries). */
const BUNDLED_URLS: Record<string, string> = Object.fromEntries(
  Object.entries(BUNDLED).map(([path, url]) => [
    path.slice(path.lastIndexOf("/") + 1, -".svg".length),
    url as unknown as string,
  ]),
);

/**
 * Bundled marks that are pure black (either `fill="#000000"` or
 * `fill="currentColor"`), so they need the dark-mode invert to stay legible.
 * Brand-tinted marks must NOT be listed — their color is the brand.
 *
 * ponytail: a hand-maintained set, because an <img> can't inherit color and
 * an svg can only be tinted by editing the asset. A new black mark needs one
 * line here; the audit command is in the commit that added this table.
 */
const MONOCHROME_BUNDLED = new Set([
  "groq", "kimi", "lmstudio", "moonshotai", "ollama", "openai",
  "siliconflow", "togetherai", "vercel", "xai", "zhipuai",
  // Generic custom-provider mark (see custom.svg). Any provider id starting
  // with `custom-` lands here via the split rule, so this entry is what makes
  // user-defined gateways resolve offline instead of two 404 round-trips.
  "custom",
]);

interface ProviderLogoProps {
  /** Canonical models.dev provider id (e.g. "anthropic", "openai"). */
  providerId: string;
  /**
   * Model id of the *selected* model (e.g. "claude-sonnet-4.5"). Matched
   * against MODEL_BRAND_PREFIX first — a model's own name is the truest
   * statement of who makes it, and it survives a custom / aggregate gateway
   * whose provider id carries no brand at all.
   */
  modelId?: string;
  /** Square edge in px. Default 14 (input menu); pass 16 for the harness list. */
  size?: number;
  className?: string;
}

/**
 * Map provider IDs that don't match Simple Icons slugs exactly. Keep entries
 * tiny — anything not listed falls back to the bare id (and gets hidden by
 * the onError handler if both sources 404).
 *
 * Verified against https://cdn.simpleicons.org/{slug} 200 responses:
 *   anthropic, google, deepseek, moonshotai, qwen, perplexity, ollama,
 *   mistralai, alibabacloud, minimax all match directly. The overrides
 *   below cover the few cases where the *base* segment doesn't match, OR
 *   where a compound id must short-circuit the split rule (otherwise the
 *   split would extract a sibling brand — see google-vertex below).
 */
const SIMPLE_ICONS_SLUG_OVERRIDES: Record<string, string> = {
  // vendor renames (models.dev id != Simple Icons slug)
  mistral: "mistralai",
  alibaba: "alibabacloud",
  // Vertex AI shares the Google Cloud brand mark (sparkle), not the
  // generic Google "G". The split rule would otherwise reduce
  // "google-vertex" → "google" and show the wrong mark.
  "google-vertex": "googlecloud",
  "google-vertex-anthropic": "googlecloud",
};

/** Resolve the Simple Icons slug for a given provider id. */
function simpleIconsSlug(providerId: string): string {
  // 1) Exact override (covers full-id matches like a future "openai-pro"
  //    that the team wants to pin to a specific slug).
  const exact = SIMPLE_ICONS_SLUG_OVERRIDES[providerId];
  if (exact) return exact;
  // 2) Strip everything after the first space / dash / underscore. This
  //    collapses compound ids like "minimax token plan",
  //    "zhipuai-coding-plan", "openai-pro-v1" to the parent brand — the
  //    Simple Icons catalog only ships parent-brand marks, so hit rate
  //    jumps from ~70% to ~95% for the long tail of provider ids.
  const firstSegment = providerId.split(/[\s\-_]/, 1)[0];
  // 3) Then re-apply overrides so e.g. "alibaba-cn" → "alibaba" →
  //    "alibabacloud" still routes right. The `-cn` variants in the
  //    CN_VARIANT_PROVIDERS list (minimax, moonshotai, alibaba, zhipuai)
  //    all flow through this path without needing explicit entries.
  return SIMPLE_ICONS_SLUG_OVERRIDES[firstSegment] ?? firstSegment;
}

/**
 * Model-name prefix → brand slug. This is the *primary* lookup for the model
 * picker: the button shows a model, so the model's own name decides the mark
 * (`claude-*` → anthropic, `gemini-*` → google). It also does the right thing
 * for a custom gateway serving someone else's models, where the provider id
 * (`my-proxy`) names no brand at all.
 *
 * Matched as "lowercased model id starts with the key", longest key first, so
 * a longer key can't be shadowed by a shorter one. Keys are bare vendor names,
 * so both `deepseek-v3` and `deepseek/deepseek-r1` land on deepseek.
 *
 * ponytail: no fuzzy / substring matching — a deliberate miss (e.g. a model
 * called `my-fork-of-gpt`) falls through to the provider mark rather than
 * guessing. Add a key here when a vendor shows up without one.
 */
const MODEL_BRAND_PREFIX: Record<string, string> = {
  // Anthropic ships under two model families
  claude: "anthropic",
  // Google
  gemini: "google",
  gemma: "google",
  // OpenAI — also the prefix on hosted variants (gpt-4o-mini, o3-pro)
  gpt: "openai",
  o1: "openai",
  o3: "openai",
  o4: "openai",
  chatgpt: "openai",
  // DeepSeek
  deepseek: "deepseek",
  // Zhipu / GLM. Neither Simple Icons nor models.dev ships a legible GLM
  // mark (models.dev's is stroke-only — 1.5px lines that vanish at 12px),
  // so both model families borrow Zhipu's filled Z. Remove this line if a
  // real GLM mark ever lands in provider-logos/.
  glm: "zhipuai",
  chatglm: "zhipuai",
  // Moonshot / Kimi. Kimi is Moonshot's only live model line, so every Moonshot
  // model shows the Kimi mark — `moonshot-v1-8k` included. The Moonshot AI
  // wordmark stays bundled because the provider *section header* resolves with
  // no modelId and has to name the company.
  kimi: "kimi",
  moonshot: "kimi",
  // MiniMax
  minimax: "minimax",
  // Alibaba / Qwen
  qwen: "qwen",
  qwq: "qwen",
  qvq: "qwen",
  // ByteDance / Seed. Seed and Doubao are ByteDance model brands with no mark
  // of their own (models.dev serves a generic sparkle placeholder for all
  // three), so they borrow the ByteDance mark.
  seed: "bytedance",
  doubao: "bytedance",
  // Xiaomi / MiMo
  mimo: "xiaomi",
  // Tencent / Hunyuan
  hunyuan: "tencenthy",
  // Mistral + its fine-tunes
  mistral: "mistralai",
  mixtral: "mistralai",
  magistral: "mistralai",
  codestral: "mistralai",
  // Meta
  llama: "meta",
  // xAI
  grok: "xai",
  // Perplexity
  sonar: "perplexity",
};

/** Model-name prefixes ordered longest-first so `claude` outranks `o1`-likes. */
const MODEL_PREFIXES_BY_LENGTH = Object.keys(MODEL_BRAND_PREFIX).sort(
  (a, b) => b.length - a.length,
);

/**
 * Resolve which bundled mark a (provider, model) pair should show, or null
 * when neither resolves offline. Model name first, provider as fallback —
 * that's the priority the picker button wants.
 *
 * Exported for tests: the rendered <img> src is a hashed path (or a data URI
 * once the asset is small enough to inline), so the slug is the only stable
 * thing to assert on.
 */
export function resolveBundledSlug(providerId: string, modelId?: string): string | null {
  // 1) The model speaks for itself.
  if (modelId) {
    // Aggregate gateways prefix ids as `vendor/model` (`deepseek/deepseek-r1`);
    // both halves are worth a look, and the tail is the more specific one.
    for (const candidate of [modelId.split("/").pop()!, modelId]) {
      const lower = candidate.toLowerCase();
      const hit = MODEL_PREFIXES_BY_LENGTH.find((p) => lower.startsWith(p));
      const slug = hit && MODEL_BRAND_PREFIX[hit];
      if (slug && BUNDLED_URLS[slug]) return slug;
    }
  }
  // 2) Nothing in the model name — fall back to whoever serves it.
  const providerSlug = simpleIconsSlug(providerId);
  return BUNDLED_URLS[providerSlug] ? providerSlug : null;
}

export function ProviderLogo({ providerId, modelId, size = 14, className }: ProviderLogoProps) {
  // Tier 0: bundled. Resolved synchronously (no 404 round-trip), so the
  // picker icon never flashes empty on start or on a cold CDN.
  const slug = resolveBundledSlug(providerId, modelId);
  if (slug) {
    return (
      <img
        src={BUNDLED_URLS[slug]}
        alt=""
        width={size}
        height={size}
        decoding="async"
        className={cn("shrink-0", MONOCHROME_BUNDLED.has(slug) && "dark:invert", className)}
      />
    );
  }

  // Remote chain for the long tail of provider ids (~180 of the 222 in the
  // catalog). Both tiers go through the Gateway, NOT models.dev / Simple
  // Icons directly: those upstreams send `max-age=0, must-revalidate`, so a
  // direct `<img>` re-fetches on every single menu open and the list visibly
  // fills in row by row. `/api/providers/{id}/logo` relays the bytes with
  // `immutable`, so the browser serves the second open from its own cache and
  // never asks again.
  //
  // `?src=si` picks the Simple Icons upstream, whose marks are brand-tinted;
  // the default is models.dev, whose marks are `currentColor` (black) and so
  // need the dark-mode invert the `onError` below applies. The Gateway can't
  // tell the client which upstream answered — an `<img>` never reads response
  // headers — so the tier has to be visible in the URL itself.
  const primarySrc = `${getGatewayUrl()}/api/providers/${encodeURIComponent(
    simpleIconsSlug(providerId),
  )}/logo?src=si`;
  const fallbackSrc = `${getGatewayUrl()}/api/providers/${encodeURIComponent(
    providerId,
  )}/logo`;

  return (
    <img
      src={primarySrc}
      alt=""
      width={size}
      height={size}
      // Lazy: the menu only opens occasionally and the harness list is short.
      loading="lazy"
      // decode=async avoids blocking the menu open animation on the network.
      decoding="async"
      className={cn("shrink-0", className)}
      onError={(e) => {
        // Ponytail: simple two-tier fallback chain via dataset flag so the
        // second src swap doesn't loop. Both sources 404 → hide.
        const el = e.currentTarget;
        if (!el.dataset.fallback) {
          el.dataset.fallback = "1";
          el.src = fallbackSrc;
          // models.dev SVGs use fill="currentColor" which defaults to black;
          // black-on-dark is invisible. Add invert only here (after the
          // fallback fires) so the Simple Icons brand tint stays intact.
          el.classList.add("dark:invert");
        } else {
          el.style.display = "none";
        }
      }}
    />
  );
}