/**
 * The unread dot (design §8.5).
 *
 * The mobile app has no server-side read cursor for AGENT sessions — that
 * concept exists only for user-to-user chat (`unread_count`, which is
 * authoritative and rendered as a number). So for a session the best honest
 * signal is: "the server's `message_count` is greater than the count I saw
 * last time this device looked at it."
 *
 * That is deliberately degraded, and the design says so: it cannot tell a
 * reply from the user's own message, and a second device is invisible to it.
 * Hence a DOT, never a number — a number here would be a claim the client
 * has no business making.
 *
 * Per-device, in localStorage, cleared by logout: it is a cache of what THIS
 * device has seen, which is exactly what the dot means.
 */

const KEY = 'acowor…d:v1'

type Seen = Record<string, number>

function load(): Seen {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? '{}') as Seen
  } catch {
    return {}
  }
}

function save(s: Seen): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(s))
  } catch {
    /* private mode / quota: the dot silently stops working, which is fine */
  }
}

const at = (agentId: string, sessionId: string): string => `${agentId}::${sessionId}`

/** True when the session has messages the user has not seen on this device. */
export function hasUnread(agentId: string, sessionId: string, messageCount: number | undefined): boolean {
  if (!messageCount || messageCount <= 0) return false
  const seen = load()[at(agentId, sessionId)]
  // Never-opened sessions stay quiet: a first-run inbox lighting up
  // everywhere would be noise, not information.
  if (seen === undefined) return false
  return messageCount > seen
}

/** Record the count the user is looking at — call when a session is opened. */
export function markSeen(agentId: string, sessionId: string, messageCount: number | undefined): void {
  if (!messageCount || messageCount < 0) return
  const s = load()
  s[at(agentId, sessionId)] = messageCount
  save(s)
}

/** Logout hygiene: the dot is per-device, not per-account, but a shared
 *  phone must not carry the previous user's read state forward. */
export function clearUnread(): void {
  try {
    localStorage.removeItem(KEY)
  } catch {
    /* ignore */
  }
}
