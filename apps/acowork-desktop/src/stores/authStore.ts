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
  fetchAuthPolicy,
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

/** Reload the webview so every store + MQTT listener reboots clean (§决策 7).
 *  Sets the `acowork_recovery_reload` flag so `App.tsx` skips the
 *  SplashScreen: the Gateway process is per-machine, not per-account, so
 *  switching accounts must not re-run `bootGateway` + the /health poll +
 *  the 1.5s minimum-splash linger. The recovery branch already re-registers
 *  the MQTT / fs / doc listeners that the reload destroys, which is exactly
 *  what we need here too. Without this, every account switch flashes
 *  "Connecting to Gateway..." for at least MIN_SPLASH_MS while the Gateway
 *  (already up) is re-probed for no reason. */
function reloadApp(): void {
  try {
    sessionStorage.setItem("acowork_recovery_reload", "1");
    window.location.reload();
  } catch {
    // jsdom / non-browser environment — no reload available
  }
}

/**
 * The persisted token pair, read straight from storage.
 *
 * `init()` is the only caller that needs the tokens for the auth gate itself,
 * but the Tauri auth bridge (`lib/gatewayAuthBridge.ts`) needs the pair
 * *before* `init()` runs: boot-time Rust commands (SplashScreen →
 * `ensure_system_agent`, agent installs) only carry credentials if the
 * access token is already mirrored, and the 401 rotation path reads the
 * refresh token from the store. Exposed read-only on purpose — adopting the
 * pair is `init()`'s job.
 */
export function peekStoredTokens(): { accessToken: string; refreshToken: string } | null {
  return loadTokens();
}

interface AuthStore {
  /** Resolved deployment mode; `"unknown"` until `/api/status` answers. */
  mode: AuthMode | "unknown";
  /**
   * ADR-076 §决策 6: `[multi_user].registration_open`. When set, a logged-in
   * non-admin may create accounts (the Gateway still forces `role: user`), so
   * the sidebar offers the invite affordance to everyone.
   */
  registrationOpen: boolean;
  /**
   * ADR-076 §决策 12 v2: while `true`, the Desktop renders the "Gateway
   * is not ready" gate instead of the login form. The store polls
   * `/api/status` while this flag is set; the moment it flips to
   * `false` we transition the gate into the normal flow.
   */
  setupRequired: boolean;
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
  /** ADR-076 §决策 12 v2: timer handle for the `/api/status` poll that
   * waits out first-boot setup on the Gateway host. `null` = no poll
   * in flight. */
  _setupPollHandle: ReturnType<typeof setInterval> | null;

  /** Resolve the mode and restore a stored session. Call once, after the Gateway is reachable. */
  init: () => Promise<void>;

  /**
   * ADR-076 §决策 12 v2: poll `/api/status` every 5 s while
   * `setupRequired` is `true`. The moment the Gateway flips
   * `requires_setup` to `false`, stop polling and re-run `init()` so
   * the rest of the auth flow takes over. Idempotent — calling it
   * twice does not spawn two pollers.
   */
  pollUntilSetupComplete: () => void;
  /** Stop the setup-completion poller, if any. Safe to call when no
   * poller is running. */
  stopSetupPoll: () => void;
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
  registrationOpen: false,
  setupRequired: false,
  status: "unknown",
  account: null,
  accessToken: null,
  refreshToken: null,
  error: null,
  viewAsUserId: null,
  _refreshPromise: null,
  _setupPollHandle: null,

  init: async () => {
    const url = getGatewayUrl();
    let mode: AuthMode;
    try {
      const policy = await fetchAuthPolicy(url);
      mode = policy.authMode;
      set({
        registrationOpen: policy.registrationOpen,
        setupRequired: policy.requiresSetup,
      });
    } catch (err) {
      log.warn("[authStore] failed to resolve auth mode:", err);
      // Gateway not answering the probe — leave `unknown` so the UI stays
      // out of the way, but do not gate the user behind a login.
      set({ mode: "unknown", status: "unknown" });
      return;
    }

    // ADR-076 §决策 12 v2: first-boot restricted mode short-circuits the
    // whole flow — even with a valid token in localStorage, every other
    // `/api/*` route will 403 until setup completes. Show the
    // "Gateway not ready" gate and start polling.
    if (mode === "multi_user" && get().setupRequired) {
      set({ mode, status: "setup_required" });
      get().pollUntilSetupComplete();
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

  // ADR-076 §决策 12 v2: poll loop for first-boot restricted mode. The
  // poll is intentionally light — it does NOT touch the network beyond
  // the public `/api/status` probe — so a long-running setup (operator
  // typing at the Gateway's TTY) costs us nothing.
  pollUntilSetupComplete: () => {
    if (get()._setupPollHandle !== null) return;
    const handle = setInterval(async () => {
      // Re-read setupRequired at tick-time — `stopSetupPoll` clears it.
      if (!get().setupRequired) {
        get().stopSetupPoll();
        return;
      }
      try {
        const policy = await fetchAuthPolicy(getGatewayUrl());
        set({
          setupRequired: policy.requiresSetup,
          registrationOpen: policy.registrationOpen,
        });
        if (!policy.requiresSetup) {
          get().stopSetupPoll();
          // Re-enter `init()` — same path the splash takes on a fresh
          // boot, just with `setupRequired` now false. Existing
          // localStorage token (if any) will be restored.
          await get().init();
        }
      } catch (err) {
        // Gateway might be mid-restart while the operator applies
        // changes. Log and keep polling.
        log.warn("[authStore] setup-completion probe failed:", err);
      }
    }, 5000);
    set({ _setupPollHandle: handle });
  },

  stopSetupPoll: () => {
    const handle = get()._setupPollHandle;
    if (handle !== null) {
      clearInterval(handle);
      set({ _setupPollHandle: null });
    }
  },

  setViewAsUserId: (userId) => {
    set({ viewAsUserId: userId });
  },
}));
