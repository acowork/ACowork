/**
 * Auth + boot state machine (v1.1 §7). The whole app gates on `phase`.
 *
 *   boot ──no url──▶ connect ──probe ok──▶ login ──login ok──▶ ready
 *     │                │                     ▲                   │
 *     ├─url+token─me ok──────────────────────┼──── 401 ladder ───┘
 *     ├─url+token─me fail──▶ login ──────────┘
 *     └─probe fail with saved url──▶ disconnected (retry / change address)
 *
 * `restricted` renders when `/api/status` says `requires_setup` — the
 * Gateway has no usable admin password yet and mobile never fixes that;
 * it points the user at the Desktop instead.
 *
 * Tokens live in the WebView's localStorage. The access token is 15-minute
 * lived and the refresh family is revocable server-side, so a stolen
 * device loses real value quickly; a native secure-store swap is a v1.1
 * item, not a v1 blocker.
 *
 * The 401→refresh→replay ladder is single-flight: parallel 401s share one
 * rotation, because two refreshes of the same token would trip the
 * Gateway's reuse detection and revoke the whole family.
 */

import { create } from 'zustand'
import {
  GatewayError,
  loginRequest,
  logoutRequest,
  probeStatus,
  refreshRequest,
  setAuthBridge,
  fetchMe,
} from '../lib/api'
import type { AccountMe, GatewayStatus, TokenPair } from '../lib/types'
import { clearUnread } from '../lib/unread'

export type BootPhase =
  | 'boot'
  | 'connect'
  | 'login'
  | 'restricted'
  | 'disconnected'
  | 'ready'

const LS = {
  url: 'acowork.gatewayUrl',
  access: 'acowork.accessToken',
  refresh: 'acowork.refreshToken',
  me: 'acowork.me',
}

function lsGet(k: string): string | null {
  try {
    return localStorage.getItem(k)
  } catch {
    return null
  }
}
function lsSet(k: string, v: string | null): void {
  try {
    if (v === null) localStorage.removeItem(k)
    else localStorage.setItem(k, v)
  } catch {
    /* storage is a convenience here, not a safety boundary */
  }
}

interface AuthStore {
  phase: BootPhase
  baseUrl: string
  status: GatewayStatus | null
  accessToken: string | null
  refreshToken: string | null
  /**
   * The resolved account, or null until `/auth/me` succeeds. User-to-user
   * chat addresses every route as `/api/users/{me.user_id}/…`, so this is
   * not a convenience: without it a 联系人 thread cannot be named or opened.
   */
  me: AccountMe | null
  /** Username/password error text for the login screen. */
  loginError: string
  busy: boolean

  /** Cold start: decide the phase from persisted state. */
  boot(): Promise<void>
  /** Connect screen: probe a URL, branch on auth_mode/requires_setup. */
  probe(url: string): Promise<boolean>
  login(username: string, password: string): Promise<boolean>
  logout(): Promise<void>
  /** Retry after `disconnected` — re-probes the saved URL. */
  retryConnection(): Promise<void>
  /** Back to the connect screen (change address). */
  changeAddress(): void

  // — bridge callbacks used by lib/api —
  _setTokens(p: TokenPair): void
  _refresh(): Promise<string | null>
  _sessionExpired(): void
}

let inflightRefresh: Promise<string | null> | null = null

export const useAuthStore = create<AuthStore>((set, get) => ({
  phase: 'boot',
  baseUrl: '',
  status: null,
  accessToken: null,
  refreshToken: null,
  me: null,
  loginError: '',
  busy: false,

  boot: async () => {
    const url = lsGet(LS.url)
    const access = lsGet(LS.access)
    const refresh = lsGet(LS.refresh)
    if (!url) {
      set({ phase: 'connect', baseUrl: '' })
      return
    }
    set({ baseUrl: url, accessToken: access, refreshToken: refresh })
    try {
      const st = await probeStatus(url)
      set({ status: st })
      if (st.requires_setup) {
        set({ phase: 'restricted' })
        return
      }
      if (st.auth_mode === 'local') {
        // No account system: the Gateway's single bearer token is minted by
        // Desktop; mobile v1 only supports multi_user gateways in practice,
        // but a tokenless local gateway still gets a ready state.
        set({ phase: access ? 'ready' : 'login' })
        return
      }
      if (access && refresh) {
        try {
          set({ me: await fetchMe(), phase: 'ready' })
          return
        } catch (e) {
          if (!(e instanceof GatewayError) || e.status !== 401) {
            set({ phase: 'disconnected' })
            return
          }
        }
      }
      set({ phase: 'login' })
    } catch {
      set({ phase: 'disconnected' })
    }
  },

  probe: async (url) => {
    const clean = url.trim().replace(/\/+$/, '')
    if (!/^https?:\/\//.test(clean)) {
      set({ status: null })
      return false
    }
    set({ busy: true })
    try {
      const st = await probeStatus(clean)
      lsSet(LS.url, clean)
      set({ baseUrl: clean, status: st, busy: false })
      if (st.requires_setup) {
        set({ phase: 'restricted' })
      } else {
        set({ phase: 'login' })
      }
      return true
    } catch {
      set({ busy: false, status: null })
      return false
    }
  },

  login: async (username, password) => {
    set({ busy: true, loginError: '' })
    try {
      const pair = await loginRequest(get().baseUrl, username, password)
      get()._setTokens(pair)
      // Best-effort: reaching `ready` must not hinge on the profile call. A
      // failure leaves `me` null and the screens that need it say so, rather
      // than rendering a thread that silently appears to have no messages.
      const me = await fetchMe().catch(() => null)
      set({ busy: false, phase: 'ready', me })
      return true
    } catch (e) {
      const msg =
        e instanceof GatewayError && e.status === 401
          ? '用户名或密码错误'
          : e instanceof GatewayError && e.status === 422
            ? '账号未启用或需要首次登录，请在桌面端处理'
            : '无法连接网关，请检查网络或稍后重试'
      set({ busy: false, loginError: msg })
      return false
    }
  },

  logout: async () => {
    const { baseUrl, refreshToken } = get()
    if (refreshToken) await logoutRequest(baseUrl, refreshToken)
    lsSet(LS.access, null)
    lsSet(LS.refresh, null)
    lsSet(LS.me, null)
    // The unread dot is per-device read state; a shared phone must not carry
    // it to the next account.
    clearUnread()
    set({ accessToken: null, refreshToken: null, me: null, phase: 'login', status: null })
    // Keep the saved URL: re-login should not require retyping it.
    get().boot()
  },

  retryConnection: async () => {
    const url = get().baseUrl
    set({ busy: true })
    try {
      const st = await probeStatus(url)
      set({ status: st, busy: false })
      if (st.requires_setup) set({ phase: 'restricted' })
      else if (get().accessToken && get().refreshToken) {
        try {
          await fetchMe()
          set({ phase: 'ready' })
        } catch {
          set({ phase: 'login' })
        }
      } else set({ phase: 'login' })
    } catch {
      set({ busy: false })
    }
  },

  changeAddress: () => {
    lsSet(LS.url, null)
    lsSet(LS.access, null)
    lsSet(LS.refresh, null)
    set({ baseUrl: '', accessToken: null, refreshToken: null, status: null, phase: 'connect' })
  },

  _setTokens: (p) => {
    lsSet(LS.access, p.access_token)
    lsSet(LS.refresh, p.refresh_token)
    set({ accessToken: p.access_token, refreshToken: p.refresh_token })
  },

  _refresh: async () => {
    // Single-flight: concurrent 401s must share one rotation.
    if (inflightRefresh) return inflightRefresh
    const rt = get().refreshToken
    const url = get().baseUrl
    if (!rt) return null
    inflightRefresh = (async () => {
      try {
        const pair = await refreshRequest(url, rt)
        get()._setTokens(pair)
        return pair.access_token
      } catch {
        get()._sessionExpired()
        return null
      } finally {
        inflightRefresh = null
      }
    })()
    return inflightRefresh
  },

  _sessionExpired: () => {
    lsSet(LS.access, null)
    lsSet(LS.refresh, null)
    set({ accessToken: null, refreshToken: null, phase: 'login', loginError: '登录已过期，请重新登录' })
  },
}))

// Install the transport bridge at module load: `lib/api` stays store-free
// and the cycle never forms (api → nothing, authStore → api).
setAuthBridge({
  baseUrl: () => useAuthStore.getState().baseUrl,
  accessToken: () => useAuthStore.getState().accessToken,
  refresh: () => useAuthStore.getState()._refresh(),
  sessionExpired: () => useAuthStore.getState()._sessionExpired(),
})
