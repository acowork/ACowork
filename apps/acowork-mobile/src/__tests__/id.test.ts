/**
 * The WebView-origin fallback for client ids. A throw here happens on boot
 * (the device id is read before the first screen renders), so the degraded
 * paths are the point of these tests, not the happy path.
 */
import { describe, it, expect, afterEach } from 'vitest'
import { newId } from '../lib/id'

// `globalThis.crypto` is a getter in Node/jsdom, so plain assignment throws.
const own = Object.getOwnPropertyDescriptor(globalThis, 'crypto')
function withCrypto(c: unknown): void {
  Object.defineProperty(globalThis, 'crypto', { value: c, configurable: true, writable: true })
}

describe('newId', () => {
  afterEach(() => {
    if (own) Object.defineProperty(globalThis, 'crypto', own)
    else delete (globalThis as { crypto?: unknown }).crypto
  })

  it('returns a canonical v4 shape', () => {
    expect(newId()).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  })

  it('works without randomUUID (insecure context: older Android WebView)', () => {
    // getRandomValues stays available in insecure contexts.
    withCrypto({ getRandomValues: (a: Uint8Array) => { for (let i = 0; i < a.length; i++) a[i] = i * 7; return a } })
    const id = newId()
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  })

  it('works with no crypto at all', () => {
    withCrypto(undefined)
    expect(newId()).toMatch(/^[0-9a-f-]{36}$/)
  })

  it('does not repeat ids', () => {
    expect(new Set(Array.from({ length: 200 }, newId)).size).toBe(200)
  })
})
