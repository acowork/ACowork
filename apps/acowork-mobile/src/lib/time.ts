/**
 * Timestamp formatting for the inbox and message rows (design §2.3).
 *
 * iOS Messages' convention, which is what a Chinese-market IM user expects:
 * today shows a clock, yesterday says 昨天, anything inside a week says the
 * weekday, and older shows the date. Everything is in the device's own zone
 * — there is no server timezone in this app's contract, and inventing one
 * would put every timestamp an hour off for half the users.
 */

const DAY = 86_400_000

function startOfDay(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

function clock(ms: number): string {
  const d = new Date(ms)
  const hh = d.getHours().toString().padStart(2, '0')
  const mm = d.getMinutes().toString().padStart(2, '0')
  return `${hh}:${mm}`
}

/** Compact label for a list row's trailing slot. */
export function formatListTime(ms: number | undefined | null): string {
  if (!ms || !Number.isFinite(ms)) return ''
  const days = Math.round((startOfDay(Date.now()) - startOfDay(ms)) / DAY)
  const d = new Date(ms)
  if (days <= 0) return clock(ms)
  if (days === 1) return '昨天'
  if (days < 7) return ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][d.getDay()]!
  const y = d.getFullYear()
  const md = `${d.getMonth() + 1}/${d.getDate()}`
  return y === new Date().getFullYear() ? md : `${y}/${md}`
}

/** Full label for a message row: date plus clock, always unambiguous. */
export function formatMessageTime(ms: number | undefined | null): string {
  if (!ms || !Number.isFinite(ms)) return ''
  const d = new Date(ms)
  const days = Math.round((startOfDay(Date.now()) - startOfDay(d.getTime())) / DAY)
  if (days <= 0) return clock(d.getTime())
  if (days === 1) return `昨天 ${clock(d.getTime())}`
  return `${d.getMonth() + 1}月${d.getDate()}日 ${clock(d.getTime())}`
}

/**
 * A day separator between messages, the IM convention that keeps a long
 * thread scannable. Empty string when the previous message is the same day.
 */
export function daySeparator(prevMs: number | null, ms: number): string | null {
  const cur = new Date(ms)
  if (prevMs !== null && startOfDay(prevMs) === startOfDay(ms)) return null
  const days = Math.round((startOfDay(Date.now()) - startOfDay(ms)) / DAY)
  if (days <= 0) return '今天'
  if (days === 1) return '昨天'
  return `${cur.getFullYear()}/${cur.getMonth() + 1}/${cur.getDate()}`
}
