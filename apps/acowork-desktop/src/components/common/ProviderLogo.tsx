//! ProviderLogo — fetches a provider mark from a two-tier fallback chain:
//!   1. https://cdn.simpleicons.org/{slug}     (brand-colored, CC0)
//!   2. https://models.dev/logos/{id}.svg      (monochrome currentColor, fallback)
//!
//! If both 404 the <img> hides itself; callers don't need a fallback.
//!
//! Used by:
//!   - ChatPanel ModelMenu (input box)        — provider section header
//!   - HarnessPage ProviderPicker             — row leading slot

import { cn } from "../../lib/utils";

interface ProviderLogoProps {
  /** Canonical models.dev provider id (e.g. "anthropic", "openai"). */
  providerId: string;
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

export function ProviderLogo({ providerId, size = 14, className }: ProviderLogoProps) {
  // Tier 1: brand-colored Simple Icons. Tier 2: models.dev monochrome
  // (fill="currentColor" → defaults to black; needs invert in dark mode).
  const primarySrc = `https://cdn.simpleicons.org/${simpleIconsSlug(providerId)}`;
  const fallbackSrc = `https://models.dev/logos/${providerId}.svg`;

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