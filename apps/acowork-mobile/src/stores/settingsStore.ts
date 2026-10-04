/**
 * Settings store (§10).
 *
 * Appearance is the only settings group with real, device-local behaviour in
 * v1, so it is the only group with a store. Everything else the design lists
 * as present (语言, 通知) has no mobile backend in v1 and is rendered as an
 * honest disabled row rather than a toggle that silently does nothing —
 * §2.1's 诚实优于沉默.
 *
 * The store is the single writer of `localStorage` for these keys; the
 * pre-paint script in index.html reads them, so the key names are a contract
 * between the two and live in lib/theme.ts, not duplicated here.
 */

import { create } from 'zustand'
import {
  ACCENTS,
  applyAppearance,
  loadAccentId,
  loadThemePref,
  saveAccentId,
  saveThemePref,
  watchSystemTheme,
  type ThemePref,
} from '../lib/theme'

interface SettingsState {
  theme: ThemePref
  accent: string
  /** Set once by the shell after the first apply; guards double-apply. */
  started: boolean

  setTheme(pref: ThemePref): void
  setAccent(id: string): void
  start(): () => void
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  theme: loadThemePref(),
  accent: loadAccentId(),
  started: false,

  setTheme(pref) {
    saveThemePref(pref)
    set({ theme: pref })
    applyAppearance(pref, get().accent)
  },

  setAccent(id) {
    if (!ACCENTS.some((a) => a.id === id)) return
    saveAccentId(id)
    set({ accent: id })
    applyAppearance(get().theme, id)
  },

  /** Apply now, and re-apply when the OS theme flips under 跟随系统. */
  start() {
    applyAppearance(get().theme, get().accent)
    const stop = watchSystemTheme(() => {
      // Only 跟随系统 follows the OS; an explicit choice is a choice.
      if (get().theme === 'system') applyAppearance('system', get().accent)
    })
    return () => {
      stop()
    }
  },
}))
