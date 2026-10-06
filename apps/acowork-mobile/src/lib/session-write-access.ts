/**
 * ADR-076 §决策 4 — one place that answers "may I write to this session?".
 *
 * The backend is the source of truth: `can_write` on the session summary,
 * resolved by `SessionMeta::is_writable_by` (true for the owner, and for
 * admins / `local` mode regardless of owner). The frontend must NOT
 * re-derive it from `visibility` — a public session is readable by
 * everyone but owned by exactly one account.
 *
 * This is a hook rather than a prop on purpose (same reasoning as the
 * Desktop twin in `apps/acowork-desktop/src/lib/session-write-access.ts`):
 * a control that renders its own gate cannot be forgotten, a control that
 * waits to be told can be. Mobile's approved-action list is smaller than
 * Desktop's, which is exactly the case where an un-gated control would go
 * unnoticed until it hit a 403.
 *
 * Defaults to `false` (i.e. writable) when the answer is unknown: a session
 * created optimistically and not yet in the list, and an older Runtime that
 * omits the field, both degrade to "enabled, backend rejects the write"
 * rather than locking every control.
 */

import { useMemo } from 'react'
import { useAgentStore } from '../stores/agentStore'

/**
 * Raw predicate over a `SessionInfo.can_write` value. Split out so tests
 * (and any non-React caller holding only the row) can exercise the exact
 * rule the UI uses, including the `undefined` → writable degradation.
 */
export function isReadOnlySession(canWrite: boolean | undefined): boolean {
  return canWrite === false
}

/** Read-only status of an arbitrary session row, for one-row-per-session UIs. */
export function useSessionReadOnly(agentId: string | null, sessionId: string | null): boolean {
  return useAgentStore((s) => {
    if (!agentId || !sessionId) return false
    const sessions = s.agents[agentId]?.sessions
    return isReadOnlySession(sessions?.find((x) => x.session_id === sessionId)?.can_write)
  })
}

const EMPTY_READ_ONLY: ReadonlySet<string> = new Set()

/**
 * Ids of the sessions this account may not write, for the session switcher
 * and the drawer's session list.
 *
 * The store's `sessions` array is the selector, NOT a freshly built Set:
 * zustand v5 compares snapshots with `Object.is` and re-renders on every
 * changed result, so a Set built inside the selector would be a new identity
 * on every render → infinite loop. Memoizing on the array identity (which
 * only changes when the store actually patches it) keeps this stable.
 */
export function useReadOnlySessionIds(agentId: string | null): ReadonlySet<string> {
  const sessions = useAgentStore((s) => (agentId ? s.agents[agentId]?.sessions : undefined))
  return useMemo(() => {
    if (!sessions) return EMPTY_READ_ONLY
    let ids: Set<string> | null = null
    for (const x of sessions) {
      if (isReadOnlySession(x.can_write)) (ids ??= new Set()).add(x.session_id)
    }
    return ids ?? EMPTY_READ_ONLY
  }, [sessions])
}
