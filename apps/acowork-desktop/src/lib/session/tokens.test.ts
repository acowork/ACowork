/**
 * Unit tests for the token module — the single owner of the account pair.
 *
 * These replace the store-level refresh/proactive suites: rotation timing,
 * single-flight, the dead-session verdict and persistence are now module
 * behaviour, so they are tested without any store in the loop.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("../logger", () => ({
  log: { debug: () => {}, info: () => {}, warn: () => {}, error: () => {} },
}));

import {
  LEAD_MS,
  MAX_STALLED_ROTATIONS,
  MIN_USABLE_MS,
  acquire,
  adopt,
  clear,
  configure,
  current,
  onSessionEnd,
  onTokenChange,
  reportRejected,
  restore,
  signOut,
  __resetForTests,
} from "./tokens";

function b64url(text: string): string {
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** JWT-shaped token whose `exp` is the given absolute unix second. */
function token(expSec: number, tag = "at"): string {
  return [
    b64url('{"alg":"EdDSA","typ":"JWT"}'),
    b64url(`{"sub":"u-1","kind":"access","tag":"${tag}","exp":${expSec}}`),
    b64url("sig"),
  ].join(".");
}

const nowSec = () => Math.floor(Date.now() / 1000);

/** A pair whose access token expires `inSec` seconds from now. */
function pair(expiresInSec: number, tag = "at"): { access_token: string; refresh_token: string } {
  return {
    access_token: token(nowSec() + expiresInSec, tag),
    refresh_token: `${tag}-rt`,
  };
}

let refresh: ReturnType<typeof vi.fn>;
let logout: ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  __resetForTests();
  refresh = vi.fn(async () => pair(900, "rot"));
  logout = vi.fn(async () => {});
  configure({ refresh, logout });
});

afterEach(() => {
  __resetForTests();
  vi.useRealTimers();
});

describe("adopt / current / persistence", () => {
  it("owns the pair and mirrors the access token to callers", () => {
    const seen: (string | null)[] = [];
    onTokenChange((t) => seen.push(t));

    const p = pair(900);
    adopt(p);

    expect(current()).toBe(p.access_token);
    expect(seen).toEqual([p.access_token]);
    expect(JSON.parse(localStorage.getItem("acowork.auth.tokens")!)).toEqual({
      accessToken: p.access_token,
      refreshToken: p.refresh_token,
    });
  });

  it("restores a persisted pair and reports nothing when there is none", () => {
    const p = pair(900);
    localStorage.setItem(
      "acowork.auth.tokens",
      JSON.stringify({ accessToken: p.access_token, refreshToken: p.refresh_token }),
    );

    expect(restore()).toBe(true);
    expect(current()).toBe(p.access_token);
  });

  it("never lets a restore clobber a pair already adopted in memory", () => {
    adopt(pair(900));
    const mem = current();
    localStorage.setItem(
      "acowork.auth.tokens",
      JSON.stringify({ accessToken: "stale", refreshToken: "stale-rt" }),
    );

    restore();
    expect(current()).toBe(mem);
  });

  it("drops storage and notifies when cleared", () => {
    adopt(pair(900));
    const seen: (string | null)[] = [];
    onTokenChange((t) => seen.push(t));

    clear();

    expect(current()).toBeNull();
    expect(seen).toEqual([null]);
    expect(localStorage.getItem("acowork.auth.tokens")).toBeNull();
  });
});

describe("acquire", () => {
  it("hands back a fresh token without touching the network", async () => {
    adopt(pair(900));
    expect(await acquire()).toBe(current());
    expect(refresh).not.toHaveBeenCalled();
  });

  it("rotates first when the cached token is inside the usable window", async () => {
    adopt(pair(30)); // 30s left < MIN_USABLE_MS
    const got = await acquire();
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledWith("at-rt");
    expect(got).toBe(pair(900, "rot").access_token);
    expect(got).toBe(current());
  });

  it("hands out a token whose exp it cannot read, and lets 401 drive it", async () => {
    adopt({ access_token: "not-a-jwt", refresh_token: "rt" });
    expect(await acquire()).toBe("not-a-jwt");
    expect(refresh).not.toHaveBeenCalled();
  });

  it("returns null with no session at all", async () => {
    expect(await acquire()).toBeNull();
    expect(refresh).not.toHaveBeenCalled();
  });
});

describe("reportRejected — the single-flight contract", () => {
  it("collapses concurrent rejections of one token into a single rotation", async () => {
    // This is the invariant the whole module exists for. Two rotations of the
    // same family look like token theft to the Gateway, which then revokes
    // every family the user holds.
    adopt(pair(900));
    const stale = current()!;

    const results = await Promise.all([
      reportRejected(stale),
      reportRejected(stale),
      reportRejected(stale),
    ]);

    expect(refresh).toHaveBeenCalledTimes(1);
    for (const r of results) expect(r).toBe(current());
  });

  it("returns the already-installed token when the caller reports a stale one", async () => {
    adopt(pair(900));
    refresh.mockImplementationOnce(async () => pair(900, "rot"));
    await reportRejected(current()!);
    const rotated = current()!;

    // A late 401 carrying the pre-rotation token must not burn another one.
    expect(await reportRejected("the-old-token")).toBe(rotated);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("returns null and ends the session when the rotation is refused", async () => {
    adopt(pair(900));
    refresh.mockRejectedValueOnce(new Error("token revoked"));

    const ended = vi.fn();
    onSessionEnd(ended);

    expect(await reportRejected(current()!)).toBeNull();
    expect(ended).toHaveBeenCalledTimes(1);
    expect(current()).toBeNull();
    expect(localStorage.getItem("acowork.auth.tokens")).toBeNull();
  });

  it("does not rotate when there is no refresh token to rotate", async () => {
    expect(await reportRejected("whatever")).toBeNull();
    expect(refresh).not.toHaveBeenCalled();
  });
});

describe("scheduled rotation (relay MQTT needs a fresh token before expiry)", () => {
  it("fires shortly before exp, not before", async () => {
    const expSec = nowSec() + 900;
    adopt({ access_token: token(expSec), refresh_token: "at-rt" });
    // `exp` is whole seconds, so derive the real deadline rather than
    // assuming the full 900s survived the floor.
    const fireAt = expSec * 1000 - Date.now() - LEAD_MS;

    await vi.advanceTimersByTimeAsync(fireAt - 1);
    expect(refresh).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledWith("at-rt");
    // The rotated pair replaced the old one, and the schedule re-armed for it.
    expect(current()).not.toBe(token(expSec));
    expect(JSON.parse(localStorage.getItem("acowork.auth.tokens")!).refreshToken).toBe("rot-rt");
  });

  it("re-arms for the next cycle from the rotated token", async () => {
    adopt(pair(900));
    await vi.advanceTimersByTimeAsync((900 - LEAD_MS / 1000) * 1000);
    expect(refresh).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync((900 - LEAD_MS / 1000) * 1000);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("stops scheduling when rotations keep handing back an already-expiring token", async () => {
    refresh.mockImplementation(async () => pair(60, "stalled"));
    adopt(pair(900));

    // An hour of fake time is far past the first cycle plus the stalled
    // retries; the guard must stop scheduling rather than hammer the Gateway.
    await vi.advanceTimersByTimeAsync(3_600_000);
    const calls = refresh.mock.calls.length;
    expect(calls).toBeGreaterThan(0);
    expect(calls).toBeLessThanOrEqual(MAX_STALLED_ROTATIONS + 1);

    await vi.advanceTimersByTimeAsync(3_600_000);
    expect(refresh).toHaveBeenCalledTimes(calls);
  });

  it("disarms when the session is cleared", async () => {
    adopt(pair(900));
    clear();
    await vi.advanceTimersByTimeAsync(900_000);
    expect(refresh).not.toHaveBeenCalled();
  });

  it("schedules nothing for a token whose exp is unreadable", async () => {
    adopt({ access_token: "not-a-jwt", refresh_token: "rt" });
    await vi.advanceTimersByTimeAsync(3_600_000);
    expect(refresh).not.toHaveBeenCalled();
  });
});

describe("signOut", () => {
  it("revokes server-side and drops local state", async () => {
    adopt(pair(900));
    const rt = JSON.parse(localStorage.getItem("acowork.auth.tokens")!).refreshToken;

    await signOut();

    expect(logout).toHaveBeenCalledWith(rt);
    expect(current()).toBeNull();
    expect(localStorage.getItem("acowork.auth.tokens")).toBeNull();
  });

  it("drops the local session even when the revoke call fails", async () => {
    logout.mockRejectedValueOnce(new Error("gateway down"));
    adopt(pair(900));

    await signOut();

    expect(current()).toBeNull();
    expect(localStorage.getItem("acowork.auth.tokens")).toBeNull();
  });
});

describe("sign-out during an in-flight rotation", () => {
  it("drops the late rotation instead of resurrecting the signed-out session", async () => {
    // The race this guards: a proactive rotation is on the wire when the user
    // signs out. If the refresh then succeeds, `adopt()` must NOT write the
    // pair back — otherwise the reloaded page restores it and the user is
    // silently logged in again.
    adopt(pair(900));
    const stale = current()!;

    let resolveRefresh!: (v: { access_token: string; refresh_token: string }) => void;
    refresh.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolveRefresh = r;
        }),
    );

    const rejected = reportRejected(stale); // starts a rotation, awaits transport
    await Promise.resolve(); // let rotate suspend at `transport.refresh`
    void signOut(); // clears the session and bumps the epoch
    resolveRefresh(pair(900, "late")); // the abandoned rotation now succeeds

    expect(await rejected).toBeNull();
    expect(current()).toBeNull();
    expect(localStorage.getItem("acowork.auth.tokens")).toBeNull();
    // The late `adopt` was suppressed, so the fresh pair never reached storage.
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});

describe("session-end signal", () => {
  it("fires once per death, not on an explicit clear", async () => {
    adopt(pair(900));
    const ended = vi.fn();
    onSessionEnd(ended);

    clear();
    expect(ended).not.toHaveBeenCalled();

    // A fresh session, then a refused rotation — that is a death, not a exit.
    adopt(pair(900));
    refresh.mockRejectedValueOnce(new Error("revoked"));
    await reportRejected(current()!);
    expect(ended).toHaveBeenCalledTimes(1);
  });

  it("lets a subscriber unsubscribe", async () => {
    const ended = vi.fn();
    const off = onSessionEnd(ended);
    off();
    adopt(pair(900));
    refresh.mockRejectedValueOnce(new Error("revoked"));
    await reportRejected(current()!);
    expect(ended).not.toHaveBeenCalled();
  });
});

describe("timing constants", () => {
  it("keeps the acquire guarantee inside the schedule", () => {
    // A scheduled rotation must fire before a token can become unacquirable,
    // otherwise an idle session would hand out a stale token on wake.
    expect(MIN_USABLE_MS).toBeLessThan(LEAD_MS);
  });
});
