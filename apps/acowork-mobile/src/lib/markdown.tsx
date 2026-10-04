/**
 * The Markdown subset the chat bubbles render (design §4.3).
 *
 * Why hand-rolled instead of `marked`/`react-markdown`: every renderer in
 * that family's default configuration emits HTML, and this text is
 * UNTRUSTED — it comes out of an agent's mouth, or a stranger's DM. A
 * `dangerouslySetInnerHTML` path here is an XSS hole with the Gateway's own
 * credentials in reach. This module returns React nodes only, so there is no
 * HTML to escape and no sanitizer to keep correct. It is also ~100 lines and
 * has no dependency.
 *
 * Supported: headings, fenced code, inline code, bold, italic, links
 * (http/https/mailto only), unordered/ordered lists, blockquotes, line
 * breaks. Anything else renders literally, which is the safe default.
 */

import type { ReactNode } from 'react'

/** Schemes a chat link may use. Everything else renders as plain text. */
const SAFE_URL = /^(https?:|mailto:)/i

/**
 * Inline spans. One alternation, scanned left to right: a match consumes its
 * region, so `` `**not bold**` `` inside code stays literal.
 *
 * `_italic_` is deliberately NOT supported — it would eat the underscores in
 * `user_name` and every other identifier a chat log contains. `*italic*` is
 * enough, and Markdown's own emphasis rule agrees on ambiguity.
 */
const INLINE = /`([^`\n]+)`|\*\*([^*\n]+)\*\*|\*([^*\n]+)\*|\[([^\]\n]*)\]\(([^()\s]*)\)/g

function inline(text: string, keyBase: string): ReactNode[] {
  const out: ReactNode[] = []
  let last = 0
  let n = 0
  // A fresh regex per call: `INLINE` is /g, and sharing one across calls
  // leaks `lastIndex` between them — the classic "works alone, drops the
  // first match in a loop" bug.
  const re = new RegExp(INLINE.source, 'g')
  let m: RegExpExecArray | null
  while ((m = re.exec(text))) {
    if (!m[0].length) re.lastIndex++ // zero-width guard
    const at = m.index
    if (at > last) out.push(text.slice(last, at))
    const [all, code, bold, italic, linkText, linkHref] = m
    const key = `${keyBase}-${n++}`
    if (code !== undefined) out.push(<code key={key} className="md-icode">{code}</code>)
    else if (bold !== undefined) out.push(<strong key={key}>{inline(bold, key)}</strong>)
    else if (italic !== undefined) out.push(<em key={key}>{inline(italic, key)}</em>)
    else if (linkHref !== undefined && SAFE_URL.test(linkHref))
      out.push(
        <a key={key} className="md-link" href={linkHref} target="_blank" rel="noopener noreferrer">
          {linkText || linkHref}
        </a>,
      )
    else out.push(all) // unsupported link shape / unsafe scheme: literal
    last = at + all.length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

/**
 * Block pass. Returns one React node per block; paragraphs of several lines
 * join with <br>, which is what a chat log looks like.
 */
export function renderMarkdown(src: string): ReactNode[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n')
  const blocks: ReactNode[] = []
  let para: string[] = []
  let list: { ordered: boolean; items: string[] } | null = null
  let fence: string[] | null = null
  let key = 0

  const flushPara = () => {
    if (!para.length) return
    const parts = para.join('\n').split('\n')
    blocks.push(
      <p key={`p${key++}`} className="md-p">
        {parts.map((l, i) => (
          <span key={i}>
            {inline(l, `p${key}i${i}`)}
            {i < parts.length - 1 ? <br /> : null}
          </span>
        ))}
      </p>,
    )
    para = []
  }

  const flushList = () => {
    if (!list) return
    const items = list.items.map((it, i) => <li key={i}>{inline(it, `l${key}i${i}`)}</li>)
    blocks.push(
      list.ordered ? (
        <ol key={`l${key++}`} className="md-list">{items}</ol>
      ) : (
        <ul key={`l${key++}`} className="md-list">{items}</ul>
      ),
    )
    list = null
  }

  for (const raw of lines) {
    // Fence toggling first: inside a fence, nothing else is parsed.
    const fenceMark = /^\s*```/.test(raw)
    if (fenceMark) {
      if (fence) {
        blocks.push(
          <pre key={`c${key++}`} className="md-code">
            <code>{fence.join('\n')}</code>
          </pre>,
        )
        fence = null
      } else {
        flushPara()
        flushList()
        fence = []
      }
      continue
    }
    if (fence) {
      fence.push(raw)
      continue
    }

    let m: RegExpExecArray | null
    if (!raw.trim()) {
      flushPara()
      flushList()
      continue
    }
    if ((m = /^(#{1,6})\s+(.*)$/.exec(raw))) {
      flushPara()
      flushList()
      const level = Math.min(6, m[1]!.length + 2) // # -> h3: bubbles have no h1
      const Tag = (`h${level}`) as 'h3'
      blocks.push(<Tag key={`h${key++}`} className="md-h">{inline(m[2]!, `h${key}`)}</Tag>)
      continue
    }
    if ((m = /^\s*>\s?(.*)$/.exec(raw))) {
      flushPara()
      flushList()
      blocks.push(<blockquote key={`q${key++}`} className="md-quote">{inline(m[1]!, `q${key}`)}</blockquote>)
      continue
    }
    if ((m = /^\s*[-*+]\s+(.*)$/.exec(raw))) {
      flushPara()
      if (!list || list.ordered) {
        flushList()
        list = { ordered: false, items: [] }
      }
      list.items.push(m[1]!)
      continue
    }
    if ((m = /^\s*\d+[.)]\s+(.*)$/.exec(raw))) {
      flushPara()
      if (!list || !list.ordered) {
        flushList()
        list = { ordered: true, items: [] }
      }
      list.items.push(m[1]!)
      continue
    }
    flushList()
    para.push(raw)
  }
  if (fence) {
    // Unterminated fence (a truncated reply): render what we have as code.
    blocks.push(
      <pre key={`c${key++}`} className="md-code">
        <code>{fence.join('\n')}</code>
      </pre>,
    )
  }
  flushPara()
  flushList()
  return blocks
}
