/**
 * Proactive access-token rotation (relay-mode MQTT self-healing).
 *
 * The access token lives 15 minutes and, before this existed, was only ever
 * rotated lazily by a 401. Relay-mode MQTT cannot answer a 401: its CONNECT
 * password IS the token, and the strict remote listener drops a bad
 * credential WITHOUT CONNACK, so an expired mirror produced an endless
 * "Connection closed by peer abruptly" retry loop (observed live after a
 * hotspot switch). These tests pin the scheduling that prevents it.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../lib/config", () => ({ getGatewayUrl: () => "http://gw.test" }));
vi.mock("../lib/logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));

import {
  armProactiveRefresh,
  disarmProactiveRefresh,
  useAuthStore,
  MAX_STALLED_ROTATIONS,
  PROACTIVE_REFRESH_LEAD_MS,
  PROACTIVE_REFRESH_MIN_DELAY_MS,
} from "./authStore";

function b64url(text: string): string {
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** JWT-shaped token whose `exp` is the given absolute unix second. */
function tokenWithExpSec(expSec: number): string {
  return [
    b64url('{"alg":"EdDSA","typ":"JWT"}'),
    b64url(`{"sub":"u-1","kind":"access","exp":${expSec}}`),
    b64url("sig"),
  ].join(".");
}

const nowSec = () => Math.floor(Date.now() / 1000);

let refresh: ReturnType<typeof vi.fn<() => Promise<boolean>>>;

beforeEach(() => {
  vi.useFakeTimers();
  disarmProactiveRefresh();
  useAuthStore.setState({
    accessToken: null,
    refreshToken: null,
    _refreshPromise: null,
  });
  refresh = vi.fn<() => Promise<boolean>>(async () => true);
  useAuthStore.setState({ refreshTokens: refresh });
});

afterEach(() => {
  disarmProactiveRefresh();
  vi.useRealTimers();
});

describe("proactive access-token rotation", () => {
  it("rotates shortly before exp, not before", () => {
    const expSec = nowSec() + 600;
    useAuthStore.setState({ accessToken: tokenWithExpSec(expSec), refreshToken: "r" });

    const delay = expSec * 1000 - Date.now() - PROACTIVE_REFRESH_LEAD_MS;
    vi.advanceTimersByTime(delay - 1);
    expect(refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("floors the delay when the token is already inside the lead window", () => {
    // 60 s of life left, lead is 120 s → due immediately, but not spun.
    useAuthStore.setState({
      accessToken: tokenWithExpSec(nowSec() + 60),
      refreshToken: "r",
    });

    vi.advanceTimersByTime(PROACTIVE_REFRESH_MIN_DELAY_MS - 1);
    expect(refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("re-arms for the next cycle from the rotated token (via the store subscription)", () => {
    const firstExp = nowSec() + 600;
    useAuthStore.setState({ accessToken: tokenWithExpSec(firstExp), refreshToken: "r" });
    vi.advanceTimersByTime(firstExp * 1000 - Date.now() - PROACTIVE_REFRESH_LEAD_MS);
    expect(refresh).toHaveBeenCalledTimes(1);

    // The rotation lands a new token: the subscription must arm again.
    const secondExp = nowSec() + 900;
    refresh.mockClear();
    useAuthStore.setState({ accessToken: tokenWithExpSec(secondExp), refreshToken: "r2" });

    vi.advanceTimersByTime(secondExp * 1000 - Date.now() - PROACTIVE_REFRESH_LEAD_MS - 1);
    expect(refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("stops scheduling after too many rotations that do not extend the token", () => {
    // Each cycle lands a token that is still inside the lead window —
    // a rotation storm would hammer the Gateway, so it must give up.
    for (let i = 0; i < MAX_STALLED_ROTATIONS; i++) {
      useAuthStore.setState({
        accessToken: tokenWithExpSec(nowSec() + 60),
        refreshToken: `r${i}`,
      });
      vi.advanceTimersByTime(PROACTIVE_REFRESH_MIN_DELAY_MS);
    }
    expect(refresh).toHaveBeenCalledTimes(MAX_STALLED_ROTATIONS);

    // Next stalled token: no timer armed any more.
    useAuthStore.setState({ accessToken: tokenWithExpSec(nowSec() + 60), refreshToken: "rX" });
    vi.advanceTimersByTime(PROACTIVE_REFRESH_MIN_DELAY_MS * 10);
    expect(refresh).toHaveBeenCalledTimes(MAX_STALLED_ROTATIONS);
  });

  it("disarms when the token is cleared (logout)", () => {
    useAuthStore.setState({ accessToken: tokenWithExpSec(nowSec() + 600), refreshToken: "r" });
    useAuthStore.setState({ accessToken: null, refreshToken: null });

    vi.advanceTimersByTime(3_600_000);
    expect(refresh).not.toHaveBeenCalled();
  });

  it("leaves an unreadable token to the lazy 401 path", () => {
    armProactiveRefresh("not-a-token");

    vi.advanceTimersByTime(3_600_000);
    expect(refresh).not.toHaveBeenCalled();
  });
});
