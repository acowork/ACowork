/**
 * Review queue (§10): the documents an agent asked to change.
 *
 * This is the one place in the app where a human decision gates an agent's
 * write, so the list is the pending set only — approved and rejected history
 * lives on Desktop. Showing resolved requests here invites re-reviewing
 * something the server already closed.
 */

import { useEffect } from 'react'
import { useNavStore } from '../../stores/navStore'
import { useDocStore } from '../../stores/docStore'
import { useForegroundRefresh } from '../../lib/foreground'
import { ListRow, Banner } from '../../components/ui'
import { EdgeSwipe } from '../../components/EdgeSwipe'
import { formatListTime } from '../../lib/time'

export function DocReviewScreen() {
  const requests = useDocStore((s) => s.requests)
  const loading = useDocStore((s) => s.requestsLoading)
  const error = useDocStore((s) => s.error)
  const refresh = useDocStore((s) => s.refreshRequests)
  const openRequest = useDocStore((s) => s.openRequest)
  const pop = useNavStore((s) => s.pop)
  const push = useNavStore((s) => s.push)

  useEffect(() => {
    void refresh()
  }, [refresh])
  useForegroundRefresh(() => void refresh())

  const open = (id: string) => {
    void openRequest(id)
    push('docs/request')
  }

  return (
    <EdgeSwipe onBack={() => pop()}>
      <div className="screen">
        <header className="navbar">
          <button type="button" className="navbar-back" onClick={() => pop()} aria-label="返回">返回</button>
          <span className="navbar-title">审阅队列</span>
          <span className="navbar-trail" />
        </header>

        {error ? (
          <div style={{ padding: 'var(--space-2) var(--space-4)' }}>
            <Banner tone="error">{error}</Banner>
          </div>
        ) : null}

        <div className="scroll">
          {(requests ?? []).map((r) => (
            <ListRow
              key={r.request_id}
              arrow
              onClick={() => open(r.request_id)}
              label={<span className="row-title">{r.path}</span>}
              hint={`基于 v${r.base_version} · ${r.submitted_by}`}
              value={formatListTime(Date.parse(r.created_at))}
            />
          ))}
          {loading && !requests ? <ListRow label="载入中…" /> : null}
          {requests && requests.length === 0 ? <ListRow label="没有待审阅的修改" /> : null}
        </div>
      </div>
    </EdgeSwipe>
  )
}
