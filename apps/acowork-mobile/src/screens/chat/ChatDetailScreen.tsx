/**
 * Chat detail — the busiest screen in the app, and the one carrying the
 * two invariants that are easiest to get wrong.
 *
 * 1. MULTI-SESSION (ADR-086). An agent has many sessions and the user
 *    switches between them constantly, so the switcher is the navigation
 *    bar TITLE, not a tab strip: a horizontal tab bar cannot fit on a
 *    390pt screen once real session titles are shown, and it is not the
 *    IM idiom. Tapping the title opens an Action Sheet listing every
 *    session.
 *
 * 2. READ-ONLY (ADR-076 §决策 4). `can_write === false` renders a banner,
 *    removes the composer, and disables the visibility control — the
 *    control stays visible and disabled rather than disappearing, so the
 *    user can see the session is shared and understand why they cannot
 *    post. The permission is never re-derived from `visibility`.
 */

import { useCallback, useState } from 'react'
import { useAgentStore } from '../../stores/agentStore'
import { useChatStore } from '../../stores/chatStore'
import { useNavStore } from '../../stores/navStore'
import { useSessionReadOnly } from '../../lib/session-write-access'
import { Banner, ListRow, Sheet } from '../../components/ui'
import { EdgeSwipe } from '../../components/EdgeSwipe'

export function ChatDetailScreen() {
  const agentId = useChatStore((s) => s.selectedAgentId)
  const state = useChatStore((s) => (agentId ? s.agentStates[agentId] : undefined))
  const openSession = useChatStore((s) => s.openSession)
  const createSession = useChatStore((s) => s.createSession)
  const pop = useNavStore((s) => s.pop)

  const agent = useAgentStore((s) => (agentId ? s.agents[agentId]?.info : undefined))
  const sessions = useAgentStore((s) => (agentId ? s.agents[agentId]?.sessions : undefined)) ?? []
  const activeId = state?.activeSessionId ?? null
  const readOnly = useSessionReadOnly(agentId, activeId)

  const [switcherOpen, setSwitcherOpen] = useState(false)
  const active = sessions.find((s) => s.session_id === activeId)

  const onBack = useCallback(() => {
    if (!pop()) return
  }, [pop])

  return (
    <EdgeSwipe onBack={onBack}>
      <div className="screen">
        {/* The title IS the session switcher. */}
        <header className="navbar">
          <button type="button" className="navbar-back" onClick={onBack} aria-label="返回">
            返回
          </button>
          <button
            type="button"
            className="navbar-title navbar-title-button"
            onClick={() => setSwitcherOpen(true)}
            aria-label="切换会话"
          >
            {active?.title ?? agent?.name ?? '会话'}
            <span className="chevron-down" aria-hidden />
          </button>
          <span className="navbar-trail" />
        </header>

        {readOnly ? (
          <Banner tone="info">
            只读 · 来自 {active?.owner_id ?? '其他用户'} 的共享会话
          </Banner>
        ) : null}

        <div className="scroll scroll-chat">
          {state?.loading ? <div className="empty-state">载入中…</div> : null}
          {state?.loaded && state.messages.length === 0 ? (
            <div className="empty-state">暂无消息</div>
          ) : null}
          {state?.messages.map((m) => (
            <div key={m.id} className={`bubble bubble-${m.role === 'user' ? 'self' : 'peer'}`}>
              {m.content}
            </div>
          ))}
        </div>

        {/* Read-only sessions render NO composer at all. */}
        {readOnly ? null : (
          <div className="composer">
            <textarea className="composer-input" rows={1} placeholder="发送消息" aria-label="消息输入框" />
            <button type="button" className="composer-send" aria-label="发送">
              发送
            </button>
          </div>
        )}

        <Sheet open={switcherOpen} onClose={() => setSwitcherOpen(false)} title={agent?.name}>
          {sessions.map((s) => {
            const isReadOnly = s.can_write === false
            return (
              <ListRow
                key={s.session_id}
                onClick={() => {
                  setSwitcherOpen(false)
                  if (s.session_id !== activeId) void openSession(agentId!, s.session_id)
                }}
                label={
                  <span className="row-title">
                    {s.visibility === 'public' ? <span aria-label="公开">🌐</span> : null}
                    {s.visibility === 'private' ? <span aria-label="私有">🔒</span> : null}
                    {s.title}
                    {isReadOnly ? <span className="badge">只读</span> : null}
                  </span>
                }
                value={s.session_id === activeId ? '✓' : undefined}
                hint={s.last_message ?? undefined}
              />
            )
          })}
          <ListRow
            label="＋ 新建会话"
            onClick={() => {
              setSwitcherOpen(false)
              void createSession(agentId!)
            }}
          />
        </Sheet>
      </div>
    </EdgeSwipe>
  )
}
