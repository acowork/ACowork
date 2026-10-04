/**
 * One chat bubble, shared by the agent and the human thread (design §4.3).
 *
 * Two shapes: a text bubble, rendered through the Markdown subset, and a
 * collapsed tool-call row. A tool call is NOT a bubble — it is process, not
 * speech, and on a 390pt screen a wall of tool JSON pushes the actual answer
 * off the top. It renders as one line that expands on tap.
 *
 * Timestamps are seconds on the user-chat wire and milliseconds on the
 * agent wire (conversation JSONL → epoch ms in `mapConversationEntries`), so
 * `toMs` normalizes by magnitude rather than by caller.
 */

import { useState } from 'react'
import { renderMarkdown } from '../lib/markdown'
import { formatMessageTime } from '../lib/time'
import type { ChatMessage } from '../lib/types'

/** Seconds or milliseconds → milliseconds. A ts under 1e11 is seconds. */
export function toMs(ts: number): number {
  return ts > 0 && ts < 1e11 ? ts * 1000 : ts
}

/** `payload` as written by `mapConversationEntries`. */
type ToolPayload = { type?: string; name?: string }

export function Bubble({ msg }: { msg: ChatMessage }) {
  const tool = (msg.payload ?? {}) as ToolPayload
  if (tool.type === 'tool_call') return <ToolRow msg={msg} name={tool.name ?? 'tool'} />
  const self = msg.role === 'user'
  return (
    <div className={`bubble${self ? ' bubble-self' : ' bubble-peer'}`}>
      <div className="bubble-text">{renderMarkdown(msg.content)}</div>
      <div className="bubble-time">{formatMessageTime(toMs(msg.created_at))}</div>
    </div>
  )
}

/** Collapsed tool call: one line, arguments behind a tap. */
function ToolRow({ msg, name }: { msg: ChatMessage; name: string }) {
  const [open, setOpen] = useState(false)
  return (
    <button type="button" className="tool-row" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
      <span className="tool-row-icon" aria-hidden>⚙</span>
      <span className="tool-row-name">调用 {name}</span>
      <span className="tool-row-caret">{open ? '收起' : '详情'}</span>
      {open && msg.content ? <span className="tool-row-body">{msg.content.slice(0, 2000)}</span> : null}
    </button>
  )
}
