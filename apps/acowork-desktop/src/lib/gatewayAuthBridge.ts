/**
 * Tauri-side account-session bridge (ADR-076 §决策 3).
 *
 * `installAuthFetchInterceptor` covers the webview's own `fetch` calls. The
 * Rust command layer is a *second* HTTP client on the same session and needs
 * the same credentials — but it must not own the token pair: refresh rotation
 * has no grace window, so a second rotation source would trip the Gateway's
 * reuse detection and revoke the whole token family (ADR-076 §10.1 #3).
 *
 * So Rust only *mirrors* the access token:
 *
 *   1. every change of `authStore.accessToken` is pushed to Rust through the
 *      `set_gateway_access_token` command,
 *   2. a Rust request that answers 401 emits `gateway-auth-required`
 *      (`gateway_client.rs`), which lands here,
 *   3. this side rotates once via the store's single-flight `refreshTokens()`
 *      — the very same call the fetch interceptor uses, so parallel 401s from
 *      both clients share one rotation — and step 1 mirrors the result,
 *      releasing the paused Rust request so it replays with the new token.
 *
 * A failed rotation mirrors `null`, which also releases the waiter (the
 * original 401 is surfaced instead of stalling for the full renew timeout).
 *
 * Installed once from `main.tsx`, before App renders.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAuthStore, peekStoredTokens } from "../stores/authStore";
import { log } from "./logger";

/** Mirrors `AUTH_REQUIRED_EVENT` in `src-tauri/src/gateway_client.rs`. */
const AUTH_REQUIRED_EVENT = "gateway-auth-required";

let installed = false;

function pushAccessToken(token: string | null): void {
  void invoke("set_gateway_access_token", { accessToken: token }).catch((err) => {
    // Not fatal: the request that needed the token simply surfaces its 401.
    log.warn("[gatewayAuth] failed to mirror the access token into Rust:", err);
  });
}

export function installGatewayAuthBridge(): void {
  if (installed) return;
  installed = true;

  // Adopt the persisted pair before `authStore.init()` gets to run. The boot
  // commands (SplashScreen → `init_local_gateway` / `connect_mqtt`, agent
  // installs) execute
  // before the App-level auth gate resolves, and under `multi_user` they must
  // carry a token — otherwise every one of them answers 401. The same
  // reasoning applies to `refreshTokens()`: the rotation path has to be
  // usable from the very first 401, and it reads the refresh token from the
  // store. `init()` later re-reads the same values, so this only moves the
  // adoption earlier.
  const stored = peekStoredTokens();
  if (stored) {
    useAuthStore.setState({
      accessToken: stored.accessToken,
      refreshToken: stored.refreshToken,
    });
  }

  let mirrored: string | null | undefined;
  const mirror = (token: string | null) => {
    if (token === mirrored) return;
    mirrored = token;
    pushAccessToken(token);
  };

  mirror(useAuthStore.getState().accessToken);
  useAuthStore.subscribe((state) => mirror(state.accessToken));

  // Rust stopped on a 401 and is waiting for a newer token (10s budget).
  void listen(AUTH_REQUIRED_EVENT, () => {
    void (async () => {
      const ok = await useAuthStore.getState().refreshTokens();
      if (ok) return; // the subscription above mirrors the new token
      // Rotation failed (session gone / no refresh token): release the
      // waiter explicitly. `mirror` would dedupe a `null` that is already
      // mirrored, and Rust only wakes on an epoch bump — so bypass it.
      mirrored = null;
      pushAccessToken(null);
    })();
  }).catch((err) => {
    log.warn("[gatewayAuth] failed to listen for gateway-auth-required:", err);
  });
}
