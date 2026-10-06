/**
 * A 1:1 thread with another human (design §2.3, ADR-076 §决策 8).
 *
 * Same visual grammar as the agent thread — bubbles, composer, edge-swipe
 * back — but a much shorter list of concerns: there is no session to switch,
 * no `can_write` gate, no live status, no approval card. A human chat is a
 * flat log. Rendering it through the agent machinery would have meant
 * inventing a fake session for it, which is exactly what the previous
 * version of this screen did (and why tapping a 联系人 404'd).
 *
 * Freshness is polling-only: the MQTT bridge carries agent sessions, not
 * user chat. 5s while the thread is open, plus an immediate refresh when the
 * app returns to the foreground (§11.3).
 */

import { useEffect, useRef, useState } from 'react'
import { useNavStore } from '../../stores/navStore'
import { useUserChatStore, startUserChatPoll, stopUserChatPoll } from '../../stores/userChatStore'
import { useAuthStore } from '../../stores/authStore'
import { Bubble, toMs } from '../../components/Bubble'
import { daySeparator } from '../../lib/time'
import { Banner, ListRow } from '../../components/ui'
import { EdgeSwipe } from '../../components/EdgeSwipe'
import type { ChatMessage } from '../../lib/types'

export function UserChatScreen() {
  const peer = useUserChatStore((s) => s.activePeerId)
  const peerName = useUserChatStore((s) => s.activePeerName)
  const raw = useUserChatStore((s) => s.messages)
  const loading = useUserChatStore((s) => s.loading)
  const sending = useUserChatStore((s) => s.sending)
  const error = useUserChatStore((s) => s.error)
  const send = useUserChatStore((s) => s.send)
  const pop = useNavStore((s) => s.pop)
  const me = useAuthStore((s) => s.me)

  const [draft, setDraft] = useState('')
  const scrollRef = useRef<HTMLDivElement>(null)

  // A human message and an agent message render through the same bubble, so
  // the wire shape is lifted to the shared one here, not in the store.
  const messages: ChatMessage[] = raw.map((m, i) => ({
    id: `${m.ts}-${i}`,
    role: m.from === me?.user_id ? 'user' : 'assistant',
    content: m.body,
    created_at: toMs(m.ts),
    kind: 'text',
  }))

  useEffect(() => {
    startUserChatPoll()
    return stopUserChatPoll
  }, [])

  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [messages.length])

  const submit = async () => {
    const text = draft.trim()
    if (!text || sending) return
    if (await send(text)) setDraft('')
  }

  if (!peer) {
    // Deep-linked or the store was cleared (logout): say so, never spin.
    return (
      <EdgeSwipe onBack={() => pop()}>
        <div className="screen">
          <header className="navbar">
            <button type="button" className="navbar-back" onClick={() => pop()} aria-label="返回">返回</button>
            <span className="navbar-title">联系人</span>
            <span className="navbar-trail" />
          </header>
          <div className="empty-state">该联系人会话已关闭，请返回重试</div>
        </div>
      </EdgeSwipe>
    )
  }

  let prevTs: number | null = null

  return (
    <EdgeSwipe onBack={() => pop()}>
      <div className="screen">
        <header className="navbar">
          <button type="button" className="navbar-back" onClick={() => pop()} aria-label="返回">返回</button>
          <span className="navbar-title">{peerName ?? peer}</span>
          <span className="navbar-trail" />
        </header>

        {!me ? (
          <Banner tone="error">
            <ListRow label="无法确定当前账号" hint="退出登录后重新登录即可恢复" />
          </Banner>
        ) : null}
        {error ? <Banner tone="warning">{error}</Banner> : null}

        <div className="scroll scroll-chat" ref={scrollRef}>
          {loading ? <div className="empty-state">载入中…</div> : null}
          {!loading && messages.length === 0 ? <div className="empty-state">还没有消息</div> : null}
          {messages.map((m) => {
            const sep = daySeparator(prevTs, m.created_at)
            prevTs = m.created_at
            return (
              <div key={m.id} className="msg-wrap">
                {sep ? <div className="day-sep">{sep}</div> : null}
                <Bubble msg={m} />
              </div>
            )
          })}
        </div>

        <div className="composer">
          <textarea
            className="composer-input"
            rows={1}
            placeholder="发送消息"
            aria-label="消息输入框"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
          <button
            type="button"
            className="composer-send"
            aria-label="发送"
            disabled={!draft.trim() || sending || !me}
            onClick={() => void submit()}
          >
            发送
          </button>
        </div>
      </div>
    </EdgeSwipe>
  )
}
