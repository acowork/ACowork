/**
 * Unit tests for the account session store (ADR-076 §决策 3 / 6 / 12).
 *
 * These cover the parts the login gate and the fetch interceptor depend
 * on: mode resolution (`local` is a no-op, `multi_user` gates), session
 * restore from storage, and — most important — the single-flight refresh
 * that stops parallel 401s from double-rotating the token family (the
 * Gateway treats a reused refresh token as a compromise and kills it).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../lib/config", () => ({ getGatewayUrl: () => "http://gw.test" }));
vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));

import { useAuthStore } from "./authStore";
import type { UserAccount } from "../lib/types";

const TOKENS_KEY = "acowork.auth.tokens";

const acct: UserAccount = {
  user_id: "u-1",
  username: "alice",
  display_name: "Alice",
  role: "admin",
  language: "en",
  timezone: "UTC",
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

function json(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as unknown as Response;
}

/** Route-aware fetch stub: `routes[urlSuffix]` → Response factory. */
function stubFetch(routes: Record<string, () => Response>) {
  const spy = vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    for (const [suffix, make] of Object.entries(routes)) {
      if (url.includes(suffix)) return make();
    }
    throw new Error(`unexpected fetch: ${url}`);
  });
  globalThis.fetch = spy as unknown as typeof fetch;
  return spy;
}

beforeEach(() => {
  localStorage.clear();
  useAuthStore.setState({
    mode: "unknown",
    status: "unknown",
    account: null,
    accessToken: null,
    refreshToken: null,
    error: null,
    viewAsUserId: null,
    _refreshPromise: null,
    setupRequired: false,
    registrationOpen: false,
    _setupPollHandle: null,
  });
});

describe("authStore.init", () => {
  it("resolves to disabled under local mode (no login gate)", async () => {
    stubFetch({ "/api/status": () => json({ auth_mode: "local" }) });

    await useAuthStore.getState().init();

    expect(useAuthStore.getState().mode).toBe("local");
    expect(useAuthStore.getState().status).toBe("disabled");
  });

  it("gates under multi_user with no stored session", async () => {
    stubFetch({ "/api/status": () => json({ auth_mode: "multi_user" }) });

    await useAuthStore.getState().init();

    expect(useAuthStore.getState().status).toBe("logged_out");
  });

  it("restores a stored session via /me", async () => {
    localStorage.setItem(
      TOKENS_KEY,
      JSON.stringify({ accessToken: "at", refreshToken: "rt" }),
    );
    stubFetch({
      "/api/status": () => json({ auth_mode: "multi_user" }),
      "/api/auth/me": () => json(acct),
    });

    await useAuthStore.getState().init();

    const state = useAuthStore.getState();
    expect(state.status).toBe("logged_in");
    expect(state.account?.user_id).toBe("u-1");
  });

  it("treats an older Gateway without auth_mode as local", async () => {
    stubFetch({ "/api/status": () => json({ version: "0.0.0" }) });

    await useAuthStore.getState().init();

    expect(useAuthStore.getState().status).toBe("disabled");
  });

  it("enters setup_required when requires_setup is true", async () => {
    stubFetch({
      "/api/status": () =>
        json({ auth_mode: "multi_user", requires_setup: true }),
    });

    await useAuthStore.getState().init();

    const state = useAuthStore.getState();
    expect(state.status).toBe("setup_required");
    expect(state.setupRequired).toBe(true);
    expect(state._setupPollHandle).not.toBeNull();
    // Cleanup so the interval doesn't leak across tests.
    useAuthStore.getState().stopSetupPoll();
  });

  it("ignores requires_setup=false on a multi_user gateway", async () => {
    stubFetch({
      "/api/status": () =>
        json({ auth_mode: "multi_user", requires_setup: false }),
    });

    await useAuthStore.getState().init();

    expect(useAuthStore.getState().status).toBe("logged_out");
    expect(useAuthStore.getState().setupRequired).toBe(false);
  });
});

describe("authStore.firstLogin", () => {
  it("activates the account and stores the token pair", async () => {
    stubFetch({
      "/api/auth/first-login": () =>
        json({ access_token: "at", refresh_token: "rt", token_type: "Bearer", expires_in: 900 }),
      "/api/auth/me": () => json(acct),
    });

    await useAuthStore.getState().firstLogin("invite-123", "newpass12");

    const state = useAuthStore.getState();
    expect(state.status).toBe("logged_in");
    expect(state.accessToken).toBe("at");
    expect(state.account?.user_id).toBe("u-1");
  });

  it("surfaces the Gateway error on a spent invite token", async () => {
    stubFetch({ "/api/auth/first-login": () => json({ detail: "invalid invite token" }, 401) });

    await expect(useAuthStore.getState().firstLogin("spent", "newpass12")).rejects.toThrow();

    expect(useAuthStore.getState().status).not.toBe("logged_in");
    expect(useAuthStore.getState().error).toContain("invite");
  });
});

describe("authStore.refreshTokens", () => {  it("is single-flight: concurrent calls rotate once", async () => {
    useAuthStore.setState({ refreshToken: "rt-old", status: "logged_in" });
    const spy = stubFetch({
      "/api/auth/refresh": () =>
        json({ access_token: "at-new", refresh_token: "rt-new", token_type: "Bearer", expires_in: 900 }),
    });

    const [a, b] = await Promise.all([
      useAuthStore.getState().refreshTokens(),
      useAuthStore.getState().refreshTokens(),
    ]);

    expect(a).toBe(true);
    expect(b).toBe(true);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(useAuthStore.getState().accessToken).toBe("at-new");
    expect(useAuthStore.getState().refreshToken).toBe("rt-new");
  });

  it("drops the session when the refresh is rejected", async () => {
    useAuthStore.setState({ refreshToken: "rt-old", status: "logged_in", accessToken: "at" });
    stubFetch({ "/api/auth/refresh": () => json({ detail: "revoked" }, 401) });

    const ok = await useAuthStore.getState().refreshTokens();

    expect(ok).toBe(false);
    const state = useAuthStore.getState();
    expect(state.status).toBe("logged_out");
    expect(state.accessToken).toBeNull();
    expect(localStorage.getItem(TOKENS_KEY)).toBeNull();
  });
});

describe("authStore.onGatewayUrlChanged", () => {
  it("is a no-op when newUrl equals oldUrl", async () => {
    const spy = stubFetch({ "/api/status": () => json({ auth_mode: "multi_user" }) });

    await useAuthStore
      .getState()
      .onGatewayUrlChanged("http://gw.test", "http://gw.test");

    expect(spy).not.toHaveBeenCalled();
  });

  it("keeps the session when the new URL is an alias for the same Gateway", async () => {
    useAuthStore.setState({
      refreshToken: "rt",
      accessToken: "at",
      status: "logged_in",
      account: acct,
    });
    const meSpy = vi.fn(() => json(acct));
    stubFetch({
      "/api/auth/me": meSpy,
      "/api/status": () => json({ auth_mode: "multi_user" }),
    });

    await useAuthStore
      .getState()
      .onGatewayUrlChanged("http://localhost:19876", "http://127.0.0.1:19876");

    const state = useAuthStore.getState();
    expect(state.status).toBe("logged_in");
    expect(state.accessToken).toBe("at");
    expect(meSpy).toHaveBeenCalledTimes(1);
  });

  it("drops the session when the new Gateway rejects the token with 401", async () => {
    useAuthStore.setState({
      refreshToken: "rt",
      accessToken: "at",
      status: "logged_in",
      account: acct,
    });
    stubFetch({
      "/api/auth/me": () => json({ detail: "invalid token" }, 401),
      "/api/status": () => json({ auth_mode: "multi_user" }),
    });

    await useAuthStore
      .getState()
      .onGatewayUrlChanged("http://new-gw:19876", "http://old-gw:19876");

    const state = useAuthStore.getState();
    expect(state.status).toBe("logged_out");
    expect(state.accessToken).toBeNull();
    expect(state.refreshToken).toBeNull();
    expect(localStorage.getItem(TOKENS_KEY)).toBeNull();
  });

  it("keeps the session when the probe fails with a network error", async () => {
    useAuthStore.setState({
      refreshToken: "rt",
      accessToken: "at",
      status: "logged_in",
      account: acct,
    });
    globalThis.fetch = vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    }) as unknown as typeof fetch;

    await useAuthStore
      .getState()
      .onGatewayUrlChanged("http://unreachable:19876", "http://old-gw:19876");

    const state = useAuthStore.getState();
    expect(state.accessToken).toBe("at");
    expect(state.status).toBe("logged_in");
  });

  it("skips the probe when there is no session and just re-inits", async () => {
    useAuthStore.setState({
      status: "logged_out",
      accessToken: null,
      refreshToken: null,
    });
    stubFetch({ "/api/status": () => json({ auth_mode: "multi_user" }) });

    await useAuthStore
      .getState()
      .onGatewayUrlChanged("http://new-gw:19876", "http://old-gw:19876");

    expect(useAuthStore.getState().status).toBe("logged_out");
  });
});
