/**
 * Auth-aware `fetch` (ADR-076 §决策 3).
 *
 * The Desktop talks to the Gateway through ~170 raw `fetch` call sites
 * spread across stores and components. Rather than thread an
 * `Authorization` header through every one of them (and risk silently
 * missing one — a missing header under `multi_user` is a 401 with no
 * obvious cause), we install a single interceptor on `window.fetch` that:
 *
 *   1. only touches requests aimed at the Gateway (other origins —
 *      LSP relay, the webview's own asset protocol — pass through),
 *   2. skips `/api/auth/*` (login/refresh/logout are public and the
 *      refresh call must never recurse into the 401→refresh path),
 *   3. adds `Authorization: Bearer <access_token>` unless the caller
 *      already set one,
 *   4. on a 401, rotates the token pair once (single-flight, so parallel
 *      401s cannot double-rotate and trip the Gateway's reuse detection)
 *      and replays the request; if the refresh fails, the session is
 *      dropped and the original 401 is returned.
 *
 * Under `AUTH_MODE=local` the store's mode is never `multi_user`, so the
 * interceptor is a pure pass-through (§决策 12).
 */

import { useAuthStore } from "../stores/authStore";
import { getGatewayUrl } from "./config";
import { log } from "./logger";

function requestUrl(input: RequestInfo | URL): string {
  if (typeof input === "string") return input;
  if (input instanceof URL) return input.href;
  return input.url;
}

function hasAuthHeader(headers: HeadersInit | undefined): boolean {
  if (!headers) return false;
  if (headers instanceof Headers) return headers.has("Authorization");
  if (Array.isArray(headers)) {
    return headers.some(([k]) => k.toLowerCase() === "authorization");
  }
  return Object.keys(headers).some((k) => k.toLowerCase() === "authorization");
}

function requestHeaders(input: RequestInfo | URL, init?: RequestInit): HeadersInit | undefined {
  if (init?.headers) return init.headers;
  if (input instanceof Request) return input.headers;
  return undefined;
}

/** Rebuild the (input, init) pair with an `Authorization` header attached. */
function withToken(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  token: string,
): [RequestInfo | URL, RequestInit | undefined] {
  const headers = new Headers(requestHeaders(input, init));
  headers.set("Authorization", `Bearer ${token}`);
  if (input instanceof Request) {
    return [new Request(input, { ...init, headers }), undefined];
  }
  return [input, { ...init, headers }];
}

function targetsGateway(url: string): boolean {
  const base = getGatewayUrl().replace(/\/+$/, "");
  return base.length > 0 && url.startsWith(base);
}

/**
 * Install the interceptor. Returns an uninstall function (used by tests
 * and by nothing else — the app installs it once for its lifetime).
 */
export function installAuthFetchInterceptor(): () => void {
  const original = window.fetch.bind(window);

  window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = requestUrl(input);
    const auth = useAuthStore.getState();

    if (
      auth.mode !== "multi_user" ||
      !targetsGateway(url) ||
      url.includes("/api/auth/") ||
      hasAuthHeader(requestHeaders(input, init))
    ) {
      return original(input, init);
    }

    const token = auth.accessToken;
    if (!token) return original(input, init);

    let response = await original(...withToken(input, init, token));

    if (response.status === 401) {
      const ok = await useAuthStore.getState().refreshTokens();
      if (!ok) return response;

      const fresh = useAuthStore.getState().accessToken;
      if (!fresh) return response;
      log.debug("[authFetch] replayed request after token refresh:", url);
      response = await original(...withToken(input, init, fresh));
    }

    return response;
  };

  return () => {
    window.fetch = original;
  };
}
