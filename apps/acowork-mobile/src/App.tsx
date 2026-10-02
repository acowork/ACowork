/**
 * The app shell: TabBar + screen host. Screens are looked up in a registry
 * keyed by route; an unknown route renders an explicit "not implemented"
 * placeholder rather than silently rendering nothing, so a half-built v1
 * is visible in development instead of looking like a crash.
 */

import { useNavStore, TABS, type Route } from './stores/navStore'
import { ROUTES } from './routes'

export function App() {
  const stack = useNavStore((s) => s.stacks[s.activeTab])
  const depth = (stack?.length ?? 1) - 1
  const route = stack?.[stack.length - 1] ?? 'chat/list'
  const Screen = ROUTES[route]

  return (
    <div className="app">
      <main id="screen-root" className="screen-root">
        {Screen ? (
          <Screen />
        ) : (
          <div className="empty-state">
            <div className="empty-title">未实现</div>
            <div className="empty-sub">{String(route as Route)}</div>
          </div>
        )}
      </main>
      {depth === 0 ? <TabBar /> : null}
    </div>
  )
}

function TabBar() {
  const activeTab = useNavStore((s) => s.activeTab)
  const switchTab = useNavStore((s) => s.switchTab)
  return (
    <nav className="tabbar" role="tablist" aria-label="主导航">
      {TABS.map((t) => (
        <button
          key={t.key}
          type="button"
          role="tab"
          aria-selected={t.key === activeTab}
          className={`tabbar-item${t.key === activeTab ? ' is-active' : ''}`}
          onClick={() => switchTab(t.key)}
        >
          <span className="tabbar-icon" data-icon={t.icon} aria-hidden />
          <span className="tabbar-label">{t.label}</span>
        </button>
      ))}
    </nav>
  )
}
