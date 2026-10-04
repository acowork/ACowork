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
 *
 * 3. FOREGROUND POLLING (v1.1 §8). The screen owns the poll loop's
 *    lifetime: start on mount / session switch, stop on unmount. Only the
 *    session on screen is ever polled; results appear as complete messages
 *    when the session returns to idle — no streaming, ever, in v1.
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { useAgentStore } from '../../stores/agentStore'
import { isActiveStatus, useChatStore } from '../../stores/chatStore'
import { useNavStore } from '../../stores/navStore'
import { useSessionReadOnly } from '../../lib/session-write-access'
import { setSessionVisibility } from '../../lib/api'
import { useForegroundRefresh } from '../../lib/foreground'
import { Banner, ListRow, Sheet } from '../../components/ui'
import { Bubble } from '../../components/Bubble'
import { daySeparator } from '../../lib/time'
import { AgentDrawer } from './AgentDrawer'
import { EdgeSwipe } from '../../components/EdgeSwipe'
import type { LiveStatus } from '../../lib/types'

/** §8: one status line, not a token stream. */
function statusText(s: LiveStatus): string {
  switch (s.status) {
    case 'llm_awaiting_first_chunk':
      return '正在思考…'
    case 'thinking':
      return '正在思考…'
    case 'llm_streaming':
      return '正在生成回复…'
    case 'tool_executing':
      return '正在执行工具…'
    case 'waiting_approval':
      return '等待审批：工具执行'
    case 'paused':
      return '已暂停 — 请在桌面端继续'
    case 'errored':
      return `出错：${(s as { detail?: { message?: string } }).detail?.message ?? '会话异常终止'}`
    default:
      return ''
  }
}

export function ChatDetailScreen() {
  const agentId = useChatStore((s) => s.selectedAgentId)
  const state = useChatStore((s) => (agentId ? s.agentStates[agentId] : undefined))
  const openSession = useChatStore((s) => s.openSession)
  const createSession = useChatStore((s) => s.createSession)
  const startWatching = useChatStore((s) => s.startWatching)
  const stopWatching = useChatStore((s) => s.stopWatching)
  const sendMessage = useChatStore((s) => s.sendMessage)
  const decideApproval = useChatStore((s) => s.decideApproval)
  const answerQuestion = useChatStore((s) => s.answerQuestion)
  const pop = useNavStore((s) => s.pop)
  const deleteSession = useChatStore((s) => s.deleteSession)
  const loadMoreSessions = useAgentStore((s) => s.loadMoreSessions)
  const sessionsHasMore = useAgentStore((s) => (agentId ? s.sessionsHasMore[agentId] ?? true : true))

  const agent = useAgentStore((s) => (agentId ? s.agents[agentId]?.info : undefined))
  const sessions = useAgentStore((s) => (agentId ? s.agents[agentId]?.sessions : undefined)) ?? []
  const activeId = state?.activeSessionId ?? null
  const readOnly = useSessionReadOnly(agentId, activeId)

  const [switcherOpen, setSwitcherOpen] = useState(false)
  const [drawerOpen, setDrawerOpen] = useState(false)
  /** The session whose manage sheet is open — actions bind to it, not to the
   *  active one, so deleting row A never silently deletes the open row B. */
  const [manageId, setManageId] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const active = sessions.find((s) => s.session_id === activeId)
  const scrollRef = useRef<HTMLDivElement>(null)

  // §8 watch lifetime: exactly the foreground session, exactly while on screen.
  useEffect(() => {
    if (agentId && activeId) startWatching(agentId, activeId)
    return () => stopWatching()
  }, [agentId, activeId, startWatching, stopWatching])

  // §11.3: iOS freezes timers in the background, so the data on screen is
  // simply old on return. Reload rather than waiting for the next tick — and
  // go through `openSession`, the one path that re-points the Runtime and
  // re-reads history atomically, instead of a partial refresh that could
  // leave the two halves out of step.
  useForegroundRefresh(
    useCallback(() => {
      if (!agentId) return
      void useAgentStore.getState().refreshSessions(agentId)
      if (activeId) void openSession(agentId, activeId)
    }, [agentId, activeId, openSession]),
  )

  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [state?.messages.length, state?.status])

  const onBack = useCallback(() => {
    stopWatching()
    if (!pop()) return
  }, [pop, stopWatching])

  const send = async () => {
    const text = draft.trim()
    if (!text || !agentId || !activeId || readOnly) return
    const ok = await sendMessage(agentId, activeId, text)
    if (ok) setDraft('')
  }

  const approvalRequestId =
    state?.status?.status === 'waiting_approval'
      ? state.status.detail?.request_id || state?.approvalDetail?.requestId
      : undefined
  const approval = state?.approvalDetail && approvalRequestId === state.approvalDetail.requestId ? state.approvalDetail : null
  const question = state?.question ?? null
  const [answerDraft, setAnswerDraft] = useState('')

  // No session yet (first open of a fresh agent): create on first send.
  const sendFirst = async () => {
    const text = draft.trim()
    if (!text || !agentId) return
    const sid = await createSession(agentId)
    if (!sid) return
    const ok = await sendMessage(agentId, sid, text)
    if (ok) setDraft('')
  }

  return (
    <EdgeSwipe onBack={onBack} onDrawer={() => setDrawerOpen(true)}>
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

        <div className="scroll scroll-chat" ref={scrollRef}>
          {state?.loading ? <div className="empty-state">载入中…</div> : null}
          {state?.loaded && state.messages.length === 0 && !isActiveStatus(state.status) ? (
            <div className="empty-state">还没有消息</div>
          ) : null}
          {state?.messages.map((m, i) => {
            const sep = daySeparator(i > 0 ? state.messages[i - 1]!.created_at : null, m.created_at)
            return (
              <div key={m.id} className="msg-wrap">
                {sep ? <div className="day-sep">{sep}</div> : null}
                <Bubble msg={m} />
              </div>
            )
          })}
          {/* Failed draft: kept visible with an explicit retry (edge states). */}
          {state?.pendingSend ? (
            <div className="msg-wrap">
              <div
                className={`bubble bubble-self bubble-raw bubble-failed${state.pendingSend.state === 'sending' ? ' is-sending' : ''}`}
              >
                {state.pendingSend.content}
              </div>
              {state.pendingSend.state === 'failed' ? (
                <button
                  type="button"
                  className="retry-link"
                  onClick={() => agentId && activeId && void sendMessage(agentId, activeId)}
                >
                  发送失败 · 重试
                </button>
              ) : null}
            </div>
          ) : null}
          {/* v1.1 §8: status line instead of streaming output. */}
          {state?.status && isActiveStatus(state.status) ? (
            <div className="status-line">
              <span className="status-pulse" aria-hidden />
              <span>{statusText(state.status)}</span>
            </div>
          ) : null}
          {/* Approval card: WS channel carries tool details (§8.1); the
              polling channel only knows a request is pending (§8.3). */}
          {!readOnly && approvalRequestId && agentId && activeId ? (
            <div className="list-section" role="group" aria-label="工具审批">
              <ListRow
                label={<span className="row-title">{approval ? `允许执行 ${approval.toolName}？` : '允许该工具执行？'}</span>}
                hint={approval ? approval.action || approval.riskLevel || '工具详情' : '工具详情请在桌面端查看'}
                value={
                  <span style={{ display: 'inline-flex', gap: 8 }}>
                    <button
                      type="button"
                      className="retry-link"
                      style={{ color: 'var(--color-tint)' }}
                      onClick={() => void decideApproval(agentId, activeId, approvalRequestId, false)}
                    >
                      拒绝
                    </button>
                    <button
                      type="button"
                      className="retry-link"
                      style={{ color: 'var(--color-success)' }}
                      onClick={() => void decideApproval(agentId, activeId, approvalRequestId, true)}
                    >
                      允许
                    </button>
                  </span>
                }
              />
            </div>
          ) : null}
          {/* AskQuestion card — WS channel only (polling cannot see it, §8.3). */}
          {!readOnly && question && agentId && activeId ? (
            <div className="list-section" role="group" aria-label="助手提问">
              <div className="question-card">
                <div className="question-text">{question.question}</div>
                {question.options.length > 0 ? (
                  <div className="question-options">
                    {question.options.map((opt) => (
                      <button
                        key={opt}
                        type="button"
                        className="question-option"
                        onClick={() => void answerQuestion(agentId, activeId, question.requestId, opt)}
                      >
                        {opt}
                      </button>
                    ))}
                  </div>
                ) : null}
                <div className="question-answer">
                  <input
                    className="input-field"
                    placeholder="输入回答"
                    aria-label="回答输入框"
                    value={answerDraft}
                    onChange={(e) => setAnswerDraft(e.target.value)}
                  />
                  <button
                    type="button"
                    className="retry-link"
                    style={{ color: 'var(--color-tint)' }}
                    disabled={!answerDraft.trim()}
                    onClick={() => {
                      void answerQuestion(agentId, activeId, question.requestId, answerDraft.trim())
                      setAnswerDraft('')
                    }}
                  >
                    回答
                  </button>
                </div>
              </div>
            </div>
          ) : null}
        </div>

        {/* Read-only sessions render NO composer at all. */}
        {readOnly ? null : (
          <div className="composer">
            <textarea
              className="composer-input"
              rows={1}
              placeholder={activeId ? '发送消息' : '发送第一条消息将自动新建会话'}
              aria-label="消息输入框"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
            />
            <button
              type="button"
              className="composer-send"
              aria-label="发送"
              disabled={!draft.trim() || state?.pendingSend?.state === 'sending'}
              onClick={() => void (activeId ? send() : sendFirst())}
            >
              发送
            </button>
          </div>
        )}

        <Sheet open={switcherOpen} onClose={() => setSwitcherOpen(false)} title={agent?.name}>
          {sessions.map((s) => {
            const isReadOnly = s.can_write === false
            return (
              <div key={s.session_id} className="sheet-row">
                <ListRow
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
                {/* §5.4: delete and visibility live behind a per-row menu,
                    not on the row itself — a mis-tap on a chat list must
                    never destroy a conversation. */}
                <button
                  type="button"
                  className="sheet-row-more"
                  aria-label={`管理 ${s.title}`}
                  onClick={() => {
                    setSwitcherOpen(false)
                    setManageId(s.session_id)
                  }}
                >
                  ⋯
                </button>
              </div>
            )
          })}
          <ListRow
            label="＋ 新建会话"
            onClick={() => {
              setSwitcherOpen(false)
              void createSession(agentId!)
            }}
          />
          {sessionsHasMore ? (
            <ListRow label="加载更多" onClick={() => void loadMoreSessions(agentId!)} />
          ) : null}
        </Sheet>

        {/* Per-session action sheet (§5.4). */}
        <Sheet open={!!manageId} onClose={() => setManageId(null)} title="会话操作">
          {(() => {
            const target = sessions.find((s) => s.session_id === manageId)
            if (!target) return null
            const writable = target.can_write !== false
            const isPublic = target.visibility !== 'private'
            return (
              <>
                <ListRow
                  disabled={!writable}
                  hint={writable ? undefined : '只读会话无法修改'}
                  label={isPublic ? '🔒 设为私有' : '🌐 设为公开'}
                  onClick={() => {
                    setManageId(null)
                    void setSessionVisibility(agentId!, target.session_id, isPublic ? 'private' : 'public').then(() =>
                      useAgentStore.getState().refreshSessions(agentId!),
                    )
                  }}
                />
                <ListRow
                  destructive
                  disabled={!writable}
                  hint={writable ? undefined : '只读会话无法删除'}
                  label="删除会话"
                  onClick={() => {
                    setManageId(null)
                    void deleteSession(agentId!, target.session_id)
                  }}
                />
              </>
            )
          })()}
        </Sheet>

        {agentId ? (
          <AgentDrawer open={drawerOpen} agentId={agentId} onClose={() => setDrawerOpen(false)} />
        ) : null}
      </div>
    </EdgeSwipe>
  )
}
