/**
 * Builtin avatar icon registry — same assets as Desktop.
 * Maps icon IDs (e.g. "icon-05") to bundled image URLs.
 */

const ICON_URL_MAP = import.meta.glob<string>(
  "../assets/builtin-icons/icon-*.jpg",
  { eager: true, query: "?url", import: "default" },
)

export const BUILTIN_ICONS: Record<string, string> = Object.fromEntries(
  Object.entries(ICON_URL_MAP).map(([path, url]) => {
    const match = path.match(/icon-\d+/)
    return match ? [match[0], url] : null
  }).filter(Boolean) as [string, string][],
)

/** Stable list of bundled icon IDs (sorted). */
export const BUILTIN_ICON_IDS: readonly string[] = Object.keys(BUILTIN_ICONS).sort()
