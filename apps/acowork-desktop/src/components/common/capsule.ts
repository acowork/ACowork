/**
 * capsule.ts — the one shape every top-level app pane wears.
 *
 * The chat view has worn floating "capsule" panels (rounded-xl + hairline
 * border + own surface color) since the vibrancy pass; the pm / docs /
 * extensions views instead painted their own solid `bg-page-bg` plane, so
 * those views booted straight into a flat wall with no panel outlines and
 * no vibrancy showing through. This module is the shared shell both
 * languages now draw from.
 *
 * Shape only — the surface color stays with each pane, because the four
 * existing panels deliberately differ: chat body is white, right panel is
 * one step darker (it must read as a distinct raised surface), the left
 * sidebars are `nav-surface`. Append one of `bg-chat-body` / `bg-right-panel`
 * / `bg-page-bg` / `bg-nav-surface` after this constant:
 *
 *   <div className={cn(CAPSULE_PANE_CN, "bg-chat-body")}>
 *
 * `min-h-0` is load-bearing: a flex child with `flex-col` and overflow
 * content must be able to shrink below its content height, or inner
 * scroll roots get pushed out of the panel instead of scrolling.
 *
 * Why a constant and not a Tailwind `@utility`: every consumer needs a
 * *different* surface token, so the reusable part is the shape, not a
 * full class list. A `@utility` would have to hardcode one background.
 */
export const CAPSULE_PANE_CN =
  "flex min-h-0 flex-col overflow-hidden rounded-xl border border-border-outer";
