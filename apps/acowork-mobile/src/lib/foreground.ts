/**
 * Foreground return (design §11.3).
 *
 * A phone app is backgrounded constantly, and iOS freezes timers while it
 * is: the poll loop does not tick, so the data on screen is simply old when
 * the user comes back, and the next tick may be seconds away. Every screen
 * that shows server data reloads immediately on return instead of waiting
 * for the timer to wake up.
 */

import { useEffect } from 'react'

export function useForegroundRefresh(cb: () => void): void {
  useEffect(() => {
    const handler = () => {
      if (document.visibilityState === 'visible') cb()
    }
    document.addEventListener('visibilitychange', handler)
    // iOS Safari also fires `focus` when the tab is restored from the
    // app switcher without a visibilitychange in some WebViews.
    window.addEventListener('focus', handler)
    return () => {
      document.removeEventListener('visibilitychange', handler)
      window.removeEventListener('focus', handler)
    }
  }, [cb])
}
