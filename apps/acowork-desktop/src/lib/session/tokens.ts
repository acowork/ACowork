/**
 * The single owner of the account token pair (ADR-076 §决策 3).
 *
 * Everything about the pair's *lifecycle* lives here: persistence, expiry
 * bookkeeping, rotation timing, the single-flight guard, and the verdict
 * that a session is dead. Consumers — the `authFetch` interceptor, the Tauri
 * auth bridge, `authStore` — only ever *ask* for a token or *report* that one
 * was rejected. They never decide when to rotate, and none of them holds a
 * refresh token.
 *
 * That separation is what makes the invariant enforceable rather than
 * aspirational. Rotation is single-use, so two rotations of the same family
 * look exactly like token theft to the Gateway, which then revokes every
 * family the user holds. Exactly one party may rotate and it must be able to
 * serialize its own callers. While each consumer drove its own 401→refresh
 * ladder, the guard was a store field read from three places; here it is a
 * module private that no caller can observe or route around.
 *
 * Deliberately store-free: the rotation transport arrives through
 * `configure()`. Importing `lib/config` directly would close a cycle
 * (config → settingsStore → authStore → this module).
 */

import { jwtExpMs } from "../jwt";
import { log } from "../logger";

/** Unchanged from the pre-module store, so existing sessions survive. */
const STORAGE_KEY = "acowork.auth.tokens";

/**
 * How far ahead of `exp` rotation fires. Sized to one round-trip plus clock
 * skew, well inside the 15-minute TTL.
 *
 * This is not an optimization of the 401 path — it exists because the relay
 * MQTT CONNECT password *is* the access token and a bad credential is
 * dropped without CONNACK, so that consumer cannot recover from a 401 at
 * all. Keeping the mirrored token fresh before expiry is the only fix
 * available to it.
 */
export const LEAD_MS = 120_000;

/**
 * The guarantee `acquire()` makes to its callers: the token it hands back has
 * at least this much life left, so a request that carries it cannot race
 * expiry mid-flight.
 */
export const MIN_USABLE_MS = 60_000;

/** Floor on the re-check delay, so an already-inside-lead token cannot spin. */
export const MIN_RETRY_DELAY_MS = 15_000;

/** Consecutive rotations that failed to extend the token before we give up
 *  on scheduling and fall back to the reject-driven path. */
export const MAX_STALLED_ROTATIONS = 3;

interface Pair {
  accessToken: string;
  refreshToken: string;
}

/** The wire shape `/api/auth/login` and `/api/auth/refresh` answer with. */
export interface IssuedPair {
  access_token: string;
  refresh_token: string;
}

/** Injected transport. Keeps this module free of store and config imports. */
export interface RotationTransport {
  refresh: (refreshToken: string) => Promise<IssuedPair>;
  logout: (refreshToken: string) => Promise<void>;
}

let transport: RotationTransport | null = null;
let pair: Pair | null = null;
/** Single-flight. Private and unobservable — no caller can bypass it. */
let inflight: Promise<boolean> | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
let stalledRotations = 0;
/**
 * Bumped on every teardown. A rotation captures it before its round-trip and
 * drops the result if the value changed while it was in flight, so a sign-out
 * cannot be undone by a refresh that happens to succeed after it.
 */
let epoch = 0;

const tokenListeners = new Set<(token: string | null) => void>();
const endListeners = new Set<() => void>();

function msLeft(token: string): number | null {
  const exp = jwtExpMs(token);
  return exp === null ? null : exp - Date.now();
}

function read(): Pair | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<Pair>;
    if (parsed.accessToken && parsed.refreshToken) {
      return { accessToken: parsed.accessToken, refreshToken: parsed.refreshToken };
    }
  } catch {
    // Corrupt or unavailable storage — treat as logged out.
  }
  return null;
}

function persist(): void {
  try {
    if (pair) localStorage.setItem(STORAGE_KEY, JSON.stringify(pair));
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Persistence failure is non-fatal; the session stays in memory.
  }
}

function disarm(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
}

/** Schedule the next rotation. `exp` unreadable → rely on the reject path
 *  rather than scheduling against a guess. */
function arm(): void {
  disarm();
  if (!pair) {
    stalledRotations = 0;
    return;
  }
  const left = msLeft(pair.accessToken);
  if (left === null) return;

  if (left <= LEAD_MS) {
    stalledRotations += 1;
    if (stalledRotations > MAX_STALLED_ROTATIONS) {
      log.warn(
        `[sessionTokens] giving up on scheduled rotation after ${MAX_STALLED_ROTATIONS} ` +
          "rotations that did not extend the token — relying on the 401 path",
      );
      return;
    }
  } else {
    stalledRotations = 0;
  }
  timer = setTimeout(() => {
    timer = null;
    void rotate();
  }, Math.max(MIN_RETRY_DELAY_MS, left - LEAD_MS));
}

function notifyToken(): void {
  const token = pair?.accessToken ?? null;
  for (const cb of tokenListeners) cb(token);
}

/**
 * Rotate once, serializing concurrent callers.
 *
 * Returns `false` when the session is dead — and only this function decides
 * that, from the Gateway's answer. A failed rotation is not retried eagerly:
 * the refresh token is single-use, so a retry of the same token is a reuse
 * signal. The session ends and a fresh login is the only way forward.
 */
function rotate(): Promise<boolean> {
  if (inflight) return inflight;
  if (!transport || !pair) return Promise.resolve(false);

  const refreshToken = pair.refreshToken;
  const session = epoch;
  const attempt = (async () => {
    try {
      const next = await transport!.refresh(refreshToken);
      // A sign-out during the round-trip bumped the epoch: this rotation
      // belongs to a session that is already gone. Adopting would resurrect
      // it and write the pair back to storage for the reloaded page to pick
      // up — the user silently logged back in.
      if (session !== epoch) return false;
      adopt(next);
      return true;
    } catch (err) {
      if (session !== epoch) return false; // cleared mid-flight; verdict moot
      log.warn("[sessionTokens] rotation failed — ending the session:", err);
      end();
      return false;
    } finally {
      // Only release the shared slot when this attempt still owns it. A
      // rotation that began before a `clear()` must not null the `inflight`
      // of the next session that adopts afterwards.
      if (session === epoch) inflight = null;
    }
  })();
  // Assigned before the first await of the *caller*, and the IIFE above
  // suspends at `transport.refresh` before returning here — so no second
  // caller can observe `inflight` as null while a rotation is starting.
  inflight = attempt;
  return attempt;
}

/** Supply the rotation transport. Call once, at boot. */
export function configure(deps: RotationTransport): void {
  transport = deps;
}

/** Take ownership of a freshly issued pair (login, first login, rotation). */
export function adopt(next: IssuedPair): void {
  pair = { accessToken: next.access_token, refreshToken: next.refresh_token };
  persist();
  arm();
  notifyToken();
}

/**
 * Load the persisted pair, if any. Idempotent, and never overwrites a pair
 * already adopted in memory — in-memory is the fresher copy.
 */
export function restore(): boolean {
  if (pair) return true;
  const stored = read();
  if (!stored) return false;
  pair = stored;
  arm();
  notifyToken();
  return true;
}

/** Drop the local session without touching the server.
 *  Idempotent: a no-op when there is nothing to drop, so the session-end
 *  fan-out (`end()` -> `clear()`, then a subscriber calling back in) clears
 *  and notifies exactly once instead of twice. */
export function clear(): void {
  if (!pair && inflight === null && timer === null) return;
  epoch += 1;
  pair = null;
  inflight = null;
  disarm();
  stalledRotations = 0;
  persist();
  notifyToken();
}

/** Revoke the family server-side, then drop locally. */
export async function signOut(): Promise<void> {
  const refreshToken = pair?.refreshToken ?? null;
  // Clear first: a slow or failing logout call must not leave the UI holding
  // a token the user has just signed out of.
  clear();
  if (refreshToken && transport) {
    try {
      await transport.logout(refreshToken);
    } catch (err) {
      // Best effort — a dead token is not an error worth surfacing.
      log.warn("[sessionTokens] logout request failed:", err);
    }
  }
}

/**
 * The current token, or `null`.
 *
 * Synchronous by design: the MQTT mirror and the boot-time Rust commands run
 * before any rotation can complete and need a value right now. Prefer
 * {@link acquire} where the caller can await — this makes no freshness
 * promise beyond what the scheduled rotation has already achieved.
 */
export function current(): string | null {
  return pair?.accessToken ?? null;
}

/**
 * A token usable for work that is about to start, rotating first if the
 * cached one is inside {@link MIN_USABLE_MS}. `null` means there is no
 * session — the caller sends the request unauthenticated and lets the
 * Gateway decide.
 */
export async function acquire(): Promise<string | null> {
  if (!pair) return null;
  const left = msLeft(pair.accessToken);
  // Unreadable `exp`: hand it out and let a 401 drive `reportRejected`.
  if (left === null || left > MIN_USABLE_MS) return pair.accessToken;
  await rotate();
  return pair?.accessToken ?? null;
}

/**
 * Report that the Gateway rejected `token`, and get a usable replacement.
 *
 * The caller states a fact; it does not ask for a rotation. Concurrent 401s
 * from one expired token collapse here: the first rotates, the rest find a
 * different token already installed and reuse it instead of burning a second
 * rotation of a single-use family.
 */
export async function reportRejected(token: string): Promise<string | null> {
  if (pair && pair.accessToken !== token) return pair.accessToken;
  await rotate();
  return pair?.accessToken ?? null;
}

/** Subscribe to every change of the access token, including `null`. */
export function onTokenChange(cb: (token: string | null) => void): () => void {
  tokenListeners.add(cb);
  return () => tokenListeners.delete(cb);
}

/**
 * Subscribe to "this session is gone" — the rotation was refused and no
 * retry will help. Owners of UI state react here; consumers never see this
 * because a dead session simply stops producing tokens.
 */
export function onSessionEnd(cb: () => void): () => void {
  endListeners.add(cb);
  return () => endListeners.delete(cb);
}

/** The verdict lives here, not in the callers. */
function end(): void {
  clear();
  for (const cb of endListeners) cb();
}

/** Test hook: drop the session between cases. The transport and the
 *  subscriber list are wiring, not session state — the module-load
 *  `configure()` and the store's mirror subscription must survive a reset. */
export function __resetForTests(): void {
  end();
}
