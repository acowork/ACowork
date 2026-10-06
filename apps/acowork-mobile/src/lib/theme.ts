/**
 * Appearance (§10): theme and highlight colour.
 *
 * The shell resolves "跟随系统" in JS and writes `data-theme` on <html>,
 * rather than relying on a `prefers-color-scheme` media query. That is the
 * only way an explicit light choice can override a dark OS — a media query
 * cannot be "un-matched" by an attribute.
 *
 * An inline script in index.html applies the persisted value before first
 * paint, so a dark-theme phone does not get a white flash on open; the
 * functions here are the same code path, re-run whenever the user changes a
 * setting or the OS flips while the app is open.
 */

export type ThemePref = 'system' | 'light' | 'dark'

export interface Accent {
  id: string
  label: string
  light: string
  dark: string
}

/** The iOS system colour set, restricted to what reads well as a bubble. */
export const ACCENTS: Accent[] = [
  { id: 'blue', label: '蓝', light: '#007aff', dark: '#0a84ff' },
  { id: 'purple', label: '紫', light: '#5e5ce6', dark: '#5e5ce6' },
  { id: 'green', label: '绿', light: '#30d158', dark: '#30d158' },
  { id: 'orange', label: '橙', light: '#ff9500', dark: '#ff9f0a' },
  { id: 'pink', label: '粉', light: '#ff2d55', dark: '#ff375f' },
]

const THEME_KEY = 'acowork.theme'
const ACCENT_KEY = 'acowork.accent'

export const DEFAULT_THEME: ThemePref = 'system'
export const DEFAULT_ACCENT = 'blue'

function read(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback
  } catch {
    // Private mode / a webview that refuses storage: the default is still a
    // working appearance, so this is not an error worth surfacing.
    return fallback
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value)
  } catch {
    /* ignored — see read() */
  }
}

export function loadThemePref(): ThemePref {
  const v = read(THEME_KEY, DEFAULT_THEME)
  return v === 'light' || v === 'dark' || v === 'system' ? v : DEFAULT_THEME
}

export function loadAccentId(): string {
  const v = read(ACCENT_KEY, DEFAULT_ACCENT)
  return ACCENTS.some((a) => a.id === v) ? v : DEFAULT_ACCENT
}

export function systemDark(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
}

export function resolveTheme(pref: ThemePref): 'light' | 'dark' {
  if (pref === 'system') return systemDark() ? 'dark' : 'light'
  return pref
}

/**
 * Apply both settings to the document. Idempotent, and safe to call before
 * React mounts.
 */
export function applyAppearance(pref: ThemePref, accentId: string): void {
  const resolved = resolveTheme(pref)
  const root = document.documentElement
  root.dataset.theme = resolved
  const accent = ACCENTS.find((a) => a.id === accentId) ?? ACCENTS[0]!
  const hex = resolved === 'dark' ? accent.dark : accent.light
  // Inline properties beat the stylesheet, which is what makes the accent
  // independent of which theme block is active.
  root.style.setProperty('--color-tint', hex)
  root.style.setProperty('--color-bubble-self', hex)
  // The browser chrome (status bar, form controls) follows the theme, not
  // the accent — a coloured meta theme-color is a well-known way to make an
  // app look broken on Android.
  const meta = document.querySelector('meta[name="theme-color"]')
  if (meta) meta.setAttribute('content', resolved === 'dark' ? '#000000' : '#ffffff')
}

/** Re-apply when the OS theme changes while 跟随系统 is selected. */
export function watchSystemTheme(onChange: () => void): () => void {
  if (typeof matchMedia !== 'function') return () => {}
  const mq = matchMedia('(prefers-color-scheme: dark)')
  mq.addEventListener('change', onChange)
  return () => mq.removeEventListener('change', onChange)
}

export function saveThemePref(pref: ThemePref): void {
  write(THEME_KEY, pref)
}

export function saveAccentId(id: string): void {
  write(ACCENT_KEY, id)
}
