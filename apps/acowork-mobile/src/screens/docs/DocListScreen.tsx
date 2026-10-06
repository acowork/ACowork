/**
 * Docs tab root (§10): the directory tree, plus the review queue as its own
 * entry row.
 *
 * The tree is lazily expanded — `GET /tree?dir_id=` returns one level, so a
 * directory's children are fetched the first time it opens. The prototype's
 * fully-expanded tree is a fixture convenience; on the real service an
 * eager expansion would be one request per directory on every tab switch.
 *
 * Creating and editing documents stay on Desktop (v1 is read-only here), so
 * the nav right button is the review queue rather than a plus that would
 * only be able to say "not on mobile".
 */

import { useEffect, useState } from 'react'
import { useNavStore } from '../../stores/navStore'
import { flatten, useDocStore } from '../../stores/docStore'
import { ListRow, Banner } from '../../components/ui'
import { formatListTime } from '../../lib/time'

/** 16pt per level: enough to read the indent at 390pt wide without the
 *  deepest rows losing their whole tap target to padding. */
const INDENT = 16

export function DocListScreen() {
  const levels = useDocStore((s) => s.levels)
  const open = useDocStore((s) => s.open)
  const loadingDirs = useDocStore((s) => s.loadingDirs)
  const rootLoading = useDocStore((s) => s.rootLoading)
  const error = useDocStore((s) => s.error)
  const hits = useDocStore((s) => s.hits)
  const searching = useDocStore((s) => s.searching)
  const pending = useDocStore((s) => s.requests)
  const refresh = useDocStore((s) => s.refresh)
  const toggle = useDocStore((s) => s.toggle)
  const runSearch = useDocStore((s) => s.runSearch)
  const clearSearch = useDocStore((s) => s.clearSearch)
  const openDoc = useDocStore((s) => s.openDoc)
  const refreshRequests = useDocStore((s) => s.refreshRequests)
  const push = useNavStore((s) => s.push)

  const [text, setText] = useState('')

  useEffect(() => {
    void refresh()
    // The badge is the queue's only presence on this screen, so it is
    // counted here rather than requiring a visit to the queue itself.
    void refreshRequests()
  }, [refresh, refreshRequests])

  // Debounced so typing a name does not fire a search per keystroke.
  useEffect(() => {
    const t = window.setTimeout(() => void runSearch(text), 250)
    return () => window.clearTimeout(t)
  }, [text, runSearch])

  const rows = flatten(levels, open, loadingDirs)
  const goDoc = (docId: string) => {
    void openDoc(docId)
    push('docs/read')
  }

  return (
    <div className="screen">
      <header className="navbar navbar-large">
        <h1 className="navbar-title">文档</h1>
      </header>

      <div className="search-bar">
        <input
          className="search-input"
          type="search"
          inputMode="search"
          placeholder="搜索文档"
          aria-label="搜索文档"
          value={text}
          onChange={(e) => {
            setText(e.target.value)
            void runSearch(e.target.value)
          }}
          onBlur={() => {
            if (!text.trim()) clearSearch()
          }}
        />
        {text ? (
          <button type="button" className="search-clear" aria-label="清除搜索" onClick={() => { setText(''); clearSearch() }}>
            清除
          </button>
        ) : null}
      </div>

      {error ? (
        <div style={{ padding: '0 var(--space-4)' }}>
          <Banner tone="error">{error}</Banner>
        </div>
      ) : null}

      <div className="scroll">
        {hits === null ? (
          <ListRow
            arrow
            onClick={() => push('docs/review')}
            label={<span className="row-title">审阅队列</span>}
            hint="Agent 提交的文档修改，需人工通过或驳回"
            badge={pending?.length ?? 0}
          />
        ) : null}

        <div className="list-section-title" style={{ padding: 'var(--space-3) var(--space-4) 0' }}>
          {hits === null ? '全部文档' : `搜索结果 · ${hits.length}`}
        </div>

        {hits !== null ? (
          hits.map((h) => (
            <ListRow
              key={h.doc_id}
              arrow
              onClick={() => goDoc(h.doc_id)}
              label={<span className="row-title">{h.name}</span>}
              hint={h.path}
            />
          ))
        ) : searching && rows.length === 0 ? (
          <ListRow label="载入中…" />
        ) : rows.length === 0 && !rootLoading ? (
          <ListRow label="文档库为空" hint="在桌面端创建" />
        ) : (
          rows.map((r) =>
            r.kind === 'dir' ? (
              <ListRow
                key={r.key}
                onClick={() => void toggle(r.dir_id)}
                label={
                  <span className="row-title" style={{ paddingLeft: r.depth * INDENT }}>
                    <span className="tree-caret">{r.open ? '▾' : '▸'}</span> {r.name}
                  </span>
                }
                hint={r.loading ? '载入中…' : undefined}
                value={r.count === null ? undefined : String(r.count)}
              />
            ) : (
              <ListRow
                key={r.key}
                arrow
                onClick={() => goDoc(r.doc_id)}
                label={
                  <span className="row-title" style={{ paddingLeft: r.depth * INDENT }}>
                    {r.name}
                  </span>
                }
                value={formatListTime(Date.parse(r.updated_at))}
              />
            ),
          )
        )}
        {rootLoading && rows.length === 0 && hits === null ? <ListRow label="载入中…" /> : null}
      </div>
    </div>
  )
}
