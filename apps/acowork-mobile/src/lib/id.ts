/**
 * Client-side identifiers only — the optimistic message echo key and this
 * install's device id. Neither is security-relevant.
 *
 * `crypto.randomUUID` exists only in a secure context. The Android WebView
 * serves the bundle from `http://tauri.localhost`, which current Chromium
 * treats as trustworthy but older WebViews do not — and a throw here happens
 * on boot, before any screen renders. `getRandomValues` is available even in
 * insecure contexts, so the fallback is one API level deeper, not a hack.
 */
export function newId(): string {
  const c = (globalThis as { crypto?: Crypto }).crypto
  if (typeof c?.randomUUID === 'function') return c.randomUUID()
  const b = new Uint8Array(16)
  if (c?.getRandomValues) c.getRandomValues(b)
  else for (let i = 0; i < 16; i++) b[i] = Math.floor(Math.random() * 256)
  b[6] = (b[6]! & 0x0f) | 0x40 // version 4
  b[8] = (b[8]! & 0x3f) | 0x80 // variant 10
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}
