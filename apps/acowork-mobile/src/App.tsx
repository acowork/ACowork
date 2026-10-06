/**
 * The app shell: boot gate + TabBar + screen host.
 *
 * v1.1 §7: while `authStore.phase` is not `ready`, the shell is replaced by
 * the boot screens (connect / login / disconnected / restricted). The tab
 * stack machinery is untouched — the gate sits ABOVE navigation, not inside
 * it, so a logout cannot leave a half-populated stack behind.
 *
 * Screens are looked up in a registry keyed by route; an unknown route
 * renders an explicit "not implemented" placeholder rather than silently
 * rendering nothing, so a half-built v1 is visible in development instead
 * of looking like a crash.
 */

import { useNavStore, TABS, type Route } from './stores/navStore'
import { useAuthStore } from './stores/authStore'
import { ROUTES } from './routes'
import { BootingScreen, ConnectScreen, DisconnectedScreen, LoginScreen, RestrictedScreen } from './screens/boot/Screens'

export function App() {
  const phase = useAuthStore((s) => s.phase)

  if (phase !== 'ready') {
    switch (phase) {
      case 'boot':
        return <BootingScreen />
      case 'connect':
        return <ConnectScreen />
      case 'login':
        return <LoginScreen />
      case 'disconnected':
        return <DisconnectedScreen />
      case 'restricted':
        return <RestrictedScreen />
    }
  }

  return <Shell />
}

function Shell() {
  const stack = useNavStore((s) => s.stacks[s.activeTab])
  const depth = (stack?.length ?? 1) - 1
  const route = stack?.[stack.length - 1] ?? 'chat/list'
  const Screen = ROUTES[route]

  return (
    <div className="app">
      <OfflineBanner />
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

/**
 * Global offline banner (v1.1 §7.5 edge states). Driven by the poll loop's
 * consecutive-failure count: any network failure raises it, any success
 * clears it. It never blocks navigation — reads of cached lists still work.
 */
function OfflineBanner() {
  const phase = useAuthStore((s) => s.phase)
  if (phase === 'disconnected') {
    return (
      <div className="banner banner-offline" role="status">
        连接已断开
        <button type="button" onClick={() => void useAuthStore.getState().retryConnection()}>
          重试
        </button>
      </div>
    )
  }
  return null
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
