/**
 * Account session state (ADR-076 §决策 3 / 6 / 7).
 *
 * Owns the token pair and the resolved deployment auth mode, and drives
 * the App-level login gate. Tokens are persisted to `localStorage` under
 * a single key so a webview reload keeps the session; every other store
 * is treated as derived state and rebuilt after a reload (ADR-076 §9
 * open question 6 endorses the "logged out but not logged in" mid-state
 * over a transactional switch — here that mid-state is simply a reload
 * landin on LoginView).
 *
 * `AUTH_MODE=local` (the default loopback deployment) is a full no-op
 * (§决策 12): the store resolves to `disabled` and the UI never gates.
 */

import { create } from "zustand";
import { getGatewayUrl } from "../lib/config";
import { log } from "../lib/logger";
import {
  AuthApiError,
  changePasswordRequest,
  deleteAccount,
  fetchAuthMode,
  fetchMe,
  firstLoginRequest,
  loginRequest,
  logoutRequest,
  refreshRequest,
} from "../lib/auth-api";
import type { AuthMode, AuthState, TokenPair, UserAccount } from "../lib/types";
import { useUserProfileStore } from "./userProfileStore";

const TOKENS_KEY = "acowork.auth.tokens";

interface StoredTokens {
  accessToken: string;
  refreshToken: string;
}

function loadTokens(): StoredTokens | null {
  try {
    const raw = localStorage.getItem(TOKENS_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<StoredTokens>;
    if (parsed.accessToken && parsed.refreshToken) {
      return { accessToken: parsed.accessToken, refreshToken: parsed.refreshToken };
    }
  } catch {
    // corrupted / unavailable storage — treat as logged out
  }
  return null;
}

function saveTokens(tokens: StoredTokens): void {
  try {
    localStorage.setItem(TOKENS_KEY, JSON.stringify(tokens));
  } catch {
    // persistence failure is non-fatal — session stays in-memory
  }
}

function clearStoredTokens(): void {
  try {
    localStorage.removeItem(TOKENS_KEY);
  } catch {
    // ignore
  }
}

/** Reload the webview so every store + MQTT listener reboots clean (§决策 7). */
function reloadApp(): void {
  try {
    window.location.reload();
  } catch {
    // jsdom / non-browser environment — no reload available
  }
}

interface AuthStore {
  /** Resolved deployment mode; `"unknown"` until `/api/status` answers. */
  mode: AuthMode | "unknown";
  /** App-level gate state. */
  status: AuthState;
  /** The caller's own account (redacted), once known. */
  account: UserAccount | null;
  accessToken: string | null;
  refreshToken: string | null;
  /** Last login error message, for the LoginView. */
  error: string | null;
  /**
   * ADR-076 §决策 5 / §决策 7: admin-only "view another user's sessions"
   * filter. When set, session listing appends `?as_user=<id>` (a read-only
   * view — the Gateway rejects `as_user` on writes).
   */
  viewAsUserId: string | null;
  setViewAsUserId: (userId: string | null) => void;
  /** Single-flight guard so concurrent 401s do not double-rotate. */
  _refreshPromise: Promise<boolean> | null;

  /** Resolve the mode and restore a stored session. Call once, after the Gateway is reachable. */
  init: () => Promise<void>;
  login: (username: string, password: string) => Promise<void>;
  /** ADR-076 §决策 6: consume an invite token and set the first password. */
  firstLogin: (inviteToken: string, newPassword: string) => Promise<void>;
  /** Revoke the session, clear tokens, and reload into LoginView. */
  logout: () => Promise<void>;
  /** ADR-076 §决策 7: switch account == sign out then log in as someone else. */
  switchAccount: () => Promise<void>;
  /** Change own password; the Gateway revokes all families, so this also reloads. */
  changePassword: (oldPassword: string, newPassword: string) => Promise<void>;
  /** Soft-delete own account (§决策 6), then reload into LoginView. */
  deleteSelf: () => Promise<void>;
  /** Rotate the token pair (single-flight). Returns `false` if the session is dead. */
  refreshTokens: () => Promise<boolean>;
  /** Drop the in-memory session without touching the server (used by the interceptor). */
  clearLocalSession: () => void;
}

function app(account: UserAccount | null): void {
  // §决策 7: the account is the authority; userProfileStore keeps the
  // presentation mirror the top-bar avatar reads.
  if (!account) return;
  useUserProfileStore.getState().setProfile({
    displayName: account.display_name,
    backendAvatarUrl: account.avatar ?? null,
    backendBuiltinAvatarId: account.builtin_avatar ?? null,
  });
}

export const useAuthStore = create<AuthStore>((set, get) => ({
  mode: "unknown",
  status: "unknown",
  account: null,
  accessToken: null,
  refreshToken: null,
  error: null,
  viewAsUserId: null,
  _refreshPromise: null,

  init: async () => {
    const url = getGatewayUrl();
    let mode: AuthMode;
    try {
      mode = await fetchAuthMode(url);
    } catch (err) {
      log.warn("[authStore] failed to resolve auth mode:", err);
      // Gateway not answering the probe — leave `unknown` so the UI stays
      // out of the way, but do not gate the user behind a login.
      set({ mode: "unknown", status: "unknown" });
      return;
    }

    if (mode === "local") {
      set({ mode, status: "disabled" });
      return;
    }

    const stored = loadTokens();
    if (!stored) {
      set({ mode, status: "logged_out" });
      return;
    }

    set({
      mode,
      accessToken: stored.accessToken,
      refreshToken: stored.refreshToken,
    });

    try {
      const account = await fetchMe(url, stored.accessToken);
      set({ account, status: "logged_in" });
      app(account);
    } catch (err) {
      if (err instanceof AuthApiError && err.status === 401) {
        const ok = await get().refreshTokens();
        if (!ok) return; // clearLocalSession already ran
        try {
          const account = await fetchMe(url, get().accessToken!);
          set({ account, status: "logged_in" });
          app(account);
        } catch (retryErr) {
          log.warn("[authStore] /me failed after refresh:", retryErr);
          get().clearLocalSession();
        }
      } else {
        log.warn("[authStore] session restore failed:", err);
        get().clearLocalSession();
      }
    }
  },

  login: async (username, password) => {
    const url = getGatewayUrl();
    set({ error: null });
    let pair: TokenPair;
    try {
      pair = await loginRequest(url, username, password);
    } catch (err) {
      const message =
        err instanceof AuthApiError ? err.message : "Unable to reach the Gateway";
      set({ error: message });
      throw err;
    }
    saveTokens({ accessToken: pair.access_token, refreshToken: pair.refresh_token });
    set({
      mode: "multi_user",
      status: "logged_in",
      accessToken: pair.access_token,
      refreshToken: pair.refresh_token,
      error: null,
    });
    try {
      const account = await fetchMe(url, pair.access_token);
      set({ account });
      app(account);
    } catch (err) {
      log.warn("[authStore] logged in but /me failed:", err);
    }
  },

  firstLogin: async (inviteToken, newPassword) => {
    const url = getGatewayUrl();
    set({ error: null });
    let pair: TokenPair;
    try {
      pair = await firstLoginRequest(url, inviteToken, newPassword);
    } catch (err) {
      const message =
        err instanceof AuthApiError ? err.message : "Unable to reach the Gateway";
      set({ error: message });
      throw err;
    }
    saveTokens({ accessToken: pair.access_token, refreshToken: pair.refresh_token });
    set({
      mode: "multi_user",
      status: "logged_in",
      accessToken: pair.access_token,
      refreshToken: pair.refresh_token,
      error: null,
    });
    try {
      const account = await fetchMe(url, pair.access_token);
      set({ account });
      app(account);
    } catch (err) {
      log.warn("[authStore] first login but /me failed:", err);
    }
  },

  logout: async () => {
    const refreshToken = get().refreshToken;
    clearStoredTokens();
    set({ status: "logged_out", account: null, accessToken: null, refreshToken: null });
    if (refreshToken) {
      try {
        await logoutRequest(getGatewayUrl(), refreshToken);
      } catch (err) {
        log.warn("[authStore] logout revoke failed (ignored):", err);
      }
    }
    reloadApp();
  },

  switchAccount: async () => {
    await get().logout();
  },

  changePassword: async (oldPassword, newPassword) => {
    const accessToken = get().accessToken;
    if (!accessToken) throw new AuthApiError(401, "not logged in");
    await changePasswordRequest(getGatewayUrl(), accessToken, oldPassword, newPassword);
    // §决策 6.2: all refresh families are revoked — a fresh login is required.
    clearStoredTokens();
    set({ status: "logged_out", account: null, accessToken: null, refreshToken: null });
    reloadApp();
  },

  deleteSelf: async () => {
    const { accessToken, account } = get();
    if (!accessToken || !account) throw new AuthApiError(401, "not logged in");
    await deleteAccount(getGatewayUrl(), accessToken, account.user_id);
    clearStoredTokens();
    set({ status: "logged_out", account: null, accessToken: null, refreshToken: null });
    reloadApp();
  },

  refreshTokens: async () => {
    const inFlight = get()._refreshPromise;
    if (inFlight) return inFlight;

    const refreshToken = get().refreshToken;
    if (!refreshToken) return false;

    const promise = (async () => {
      try {
        const pair = await refreshRequest(getGatewayUrl(), refreshToken);
        saveTokens({ accessToken: pair.access_token, refreshToken: pair.refresh_token });
        set({
          accessToken: pair.access_token,
          refreshToken: pair.refresh_token,
          status: "logged_in",
        });
        return true;
      } catch (err) {
        log.warn("[authStore] token refresh failed — session ended:", err);
        get().clearLocalSession();
        return false;
      } finally {
        set({ _refreshPromise: null });
      }
    })();

    set({ _refreshPromise: promise });
    return promise;
  },

  clearLocalSession: () => {
    clearStoredTokens();
    set({
      status: "logged_out",
      account: null,
      accessToken: null,
      refreshToken: null,
      viewAsUserId: null,
    });
  },

  setViewAsUserId: (userId) => {
    set({ viewAsUserId: userId });
  },
}));
