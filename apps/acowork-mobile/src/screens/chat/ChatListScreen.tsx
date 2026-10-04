/**
 * Chat tab root: the unified IM conversation stream.
 *
 * Desktop splits this into an AgentList and a UserList side by side. On a
 * 390pt-wide screen there is no room for two lists, and IM users expect
 * ONE inbox. So agents and contacts become two `ListSection` groups inside
 * a single scroll view — same data, one column, native grouping.
 *
 * Ordering: active conversations first (by recency), then the rest of the
 * directory, so the top of the list is the thing the user wants to open.
 */

import { useEffect, useMemo } from 'react'
import { useAgentStore } from '../../stores/agentStore'
import { useChatStore } from '../../stores/chatStore'
import { useNavStore } from '../../stores/navStore'
import { ListSection, ListRow } from '../../components/ui'

export function ChatListScreen() {
  const agents = useAgentStore((s) => s.agentList)
  const users = useAgentStore((s) => s.users)
  const sessionsByAgent = useAgentStore((s) => s.agents)
  const refresh = useAgentStore((s) => s.refreshDirectory)
  const loading = useAgentStore((s) => s.directoryLoading)
  const openSession = useChatStore((s) => s.openSession)
  const push = useNavStore((s) => s.push)

  // Entering the inbox re-pulls the directory (v1.1 §8.5 list freshness).
  useEffect(() => {
    void refresh()
  }, [refresh])

  // The IM preview is the last message of the agent's most recent session —
  // not a synthesized "agent is online" status line, which is what a naive
  // port of the Desktop list would show.
  const ordered = useMemo(() => {
    return [...agents].sort((a, b) => {
      const sa = sessionsByAgent[a.id]?.sessions[0]
      const sb = sessionsByAgent[b.id]?.sessions[0]
      return (sb?.updated_at ?? 0) - (sa?.updated_at ?? 0)
    })
  }, [agents, sessionsByAgent])

  const open = async (agentId: string, sessionId: string | null) => {
    useChatStore.getState().selectAgent(agentId)
    if (sessionId) await openSession(agentId, sessionId)
    push('chat/detail')
  }

  return (
    <div className="screen">
      <header className="navbar navbar-large">
        <h1 className="navbar-title">聊天</h1>
      </header>

      <div className="scroll">
        {loading && agents.length === 0 ? <div className="empty-state">载入中…</div> : null}
        <ListSection title="Agent">
          {ordered.map((a) => {
            const recent = sessionsByAgent[a.id]?.sessions[0]
            return (
              <ListRow
                key={a.id}
                arrow
                onClick={() => void open(a.id, recent?.session_id ?? null)}
                label={
                  <span className="row-title">
                    {a.name}
                    {a.status === 'offline' ? <span className="dot" aria-label="离线" /> : null}
                  </span>
                }
                value={recent?.last_message ?? (recent?.title ?? '暂无会话')}
                hint={recent ? undefined : '打开后自动创建会话'}
              />
            )
          })}
        </ListSection>

        <ListSection title="联系人">
          {users.map((u) => (
            <ListRow
              key={u.id}
              arrow
              onClick={() => {
                // v1.1 §1.3: user-to-user chat is not wired yet — the list
                // is real, the detail is a stated gap, never a fake screen.
                useChatStore.getState().selectAgent(u.id)
                push('chat/detail')
              }}
              label={
                <span className="row-title">
                  {u.display_name}
                  {u.online ? <span className="dot dot-online" aria-label="在线" /> : null}
                </span>
              }
              value={u.last_message ?? ''}
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
