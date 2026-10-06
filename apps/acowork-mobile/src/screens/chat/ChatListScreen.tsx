/**
 * Chat tab root: the unified IM conversation stream.
 *
 * Desktop splits this into an AgentList and a UserList side by side. On a
 * 390pt-wide screen there is no room for two lists, and IM users expect
 * ONE inbox. So agents and contacts become two `ListSection` groups inside
 * a single scroll view — same data, one column, native grouping.
 *
 * Ordering is recency, active conversations first. A contact with no thread
 * yet is still listed (starting one is the point of the directory), sorted
 * below everyone who has messages.
 *
 * Two badges with two different truths (§8.5): a human thread has a
 * server-side read cursor, so its count is a NUMBER. An agent session has
 * none, so it gets a DOT and nothing more — inventing a number there would
 * be a claim this client cannot support.
 */

import { useEffect, useMemo, useState } from 'react'
import { useAgentStore } from '../../stores/agentStore'
import { useChatStore } from '../../stores/chatStore'
import { useUserChatStore } from '../../stores/userChatStore'
import { useNavStore } from '../../stores/navStore'
import { hasUnread } from '../../lib/unread'
import { formatListTime } from '../../lib/time'
import { ListSection, ListRow } from '../../components/ui'

export function ChatListScreen() {
  const agents = useAgentStore((s) => s.agentList)
  const users = useAgentStore((s) => s.users)
  const sessionsByAgent = useAgentStore((s) => s.agents)
  const refreshInbox = useAgentStore((s) => s.refreshInbox)
  const loading = useAgentStore((s) => s.directoryLoading)
  const openSession = useChatStore((s) => s.openSession)
  const push = useNavStore((s) => s.push)

  const chats = useUserChatStore((s) => s.chats)
  const refreshChats = useUserChatStore((s) => s.refreshChats)

  const [query, setQuery] = useState('')

  // Entering the inbox re-pulls directory + sessions + previews (§8.5).
  useEffect(() => {
    void refreshInbox()
    void refreshChats()
  }, [refreshInbox, refreshChats])

  const q = query.trim().toLowerCase()
  const match = (name: string, sub?: string | null) =>
    !q || name.toLowerCase().includes(q) || (sub ?? '').toLowerCase().includes(q)

  const agentRows = useMemo(() => {
    return agents
      .map((a) => {
        const sessions = sessionsByAgent[a.id]?.sessions ?? []
        // Newest by the server's own `last_active_at`, not by array order:
        // a page-1 refresh replaces the list, and its order is the server's
        // pagination order, which is not guaranteed to be recency.
        const recent = sessions.reduce<null | (typeof sessions)[number]>(
          (best, s) => (!best || (s.updated_at ?? 0) > (best.updated_at ?? 0) ? s : best),
          null,
        )
        return { a, recent }
      })
      .filter(({ a, recent }) => match(a.name, recent?.last_message ?? recent?.title))
      .sort((x, y) => (y.recent?.updated_at ?? 0) - (x.recent?.updated_at ?? 0))
  }, [agents, sessionsByAgent, q])

  const chatByPeer = useMemo(() => new Map(chats.map((c) => [c.peer_user_id, c])), [chats])

  const userRows = useMemo(() => {
    return users
      .map((u) => ({ u, chat: chatByPeer.get(u.id) }))
      .filter(({ u, chat }) => match(u.display_name, chat?.last_message_preview))
      .sort((x, y) => (y.chat?.last_active_at ?? 0) - (x.chat?.last_active_at ?? 0))
  }, [users, chatByPeer, q])

  const openAgent = async (agentId: string, sessionId: string | null) => {
    useChatStore.getState().selectAgent(agentId)
    if (sessionId) await openSession(agentId, sessionId)
    push('chat/detail')
  }

  const openUser = (peerId: string, peerName: string) => {
    void useUserChatStore.getState().openChat(peerId, peerName)
    push('chat/user')
  }

  return (
    <div className="screen">
      <header className="navbar navbar-large">
        <h1 className="navbar-title">聊天</h1>
      </header>

      <div className="searchbar">
        <input
          className="searchbar-input"
          type="search"
          inputMode="search"
          placeholder="搜索 Agent 或联系人"
          aria-label="搜索"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      <div className="scroll">
        {loading && agents.length === 0 ? <div className="empty-state">载入中…</div> : null}
        <ListSection title="Agent">
          {agentRows.map(({ a, recent }) => (
            <ListRow
              key={a.id}
              arrow
              onClick={() => void openAgent(a.id, recent?.session_id ?? null)}
              label={
                <span className="row-title">
                  {a.name}
                  {a.status === 'offline' ? <span className="dot" aria-label="离线" /> : null}
                  {recent && hasUnread(a.id, recent.session_id, recent.message_count) ? (
                    <span className="dot dot-unread" aria-label="有新消息" />
                  ) : null}
                </span>
              }
              value={recent?.last_message || recent?.title || '暂无会话'}
              hint={formatListTime(recent?.updated_at)}
            />
          ))}
          {agents.length === 0 && !loading ? <ListRow label="暂无 Agent" hint="在桌面端安装后出现在这里" /> : null}
        </ListSection>

        <ListSection title="联系人">
          {userRows.map(({ u, chat }) => (
            <ListRow
              key={u.id}
              arrow
              onClick={() => openUser(u.id, u.display_name)}
              label={
                <span className="row-title">
                  {u.display_name}
                  {u.online ? <span className="dot dot-online" aria-label="在线" /> : null}
                </span>
              }
              value={chat?.last_message_preview || '开始对话'}
              hint={formatListTime(chat ? chat.last_active_at * 1000 : undefined)}
              badge={chat && chat.unread_count > 0 ? chat.unread_count : undefined}
            />
          ))}
          {users.length === 0 && !loading ? (
            <ListRow label="暂无联系人" hint="在桌面端邀请成员后出现在这里" />
          ) : null}
        </ListSection>
      </div>
    </div>
  )
}
