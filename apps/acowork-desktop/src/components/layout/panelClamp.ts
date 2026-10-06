/**
 * panelClamp.ts — who pays when the chat-view row outgrows the viewport.
 *
 * The chat view is a flex row in which every pane is rigid (`shrink-0` plus a
 * fixed inline width) except ChatPanel (`flex-1`, `min-w-[288px]`). So
 * ChatPanel absorbs the first `available - 288` pixels of pressure on its own,
 * and once it is pinned at its minimum there is nothing left that can give:
 * opening the right panel, dragging a pane wider or shrinking the window past
 * that point makes the row overflow the window.
 *
 * The overflow is resolved here, in one place, by a fixed sacrifice order
 * rather than by "shrink whichever pane is widest":
 *
 *   1. file pane  — its content wraps and scrolls, so a lost pixel costs the
 *      least there; it is also the pane users drag to the max, i.e. the one
 *      most likely to *be* the surplus.
 *   2. right pane — only when the file pane has bottomed out, and only down to
 *      what is needed to fit. Never more: the user just clicked to open it, so
 *      shrinking it beyond the deficit contradicts the action that caused it.
 *   3. nothing    — both at their minimum means the window is simply too
 *      narrow for this combination. The row clips instead of every pane
 *      becoming unreadable; that is an honest failure, not a bug.
 *
 * AgentList is deliberately not in the ladder. It is the app's only agent
 * navigation, and width is a bad proxy for "who can pay": a 400px list is the
 * widest pane on a small window, yet giving up 140px of it destroys
 * navigability while 140px from the file pane only reflows text. Paying the
 * bill with the leftmost pane is exactly the bug this module exists to stop.
 *
 * `fitPanesToViewport` is pure and idempotent — call it with already-fitted
 * widths and it returns them untouched — so AppLayout can run it on every
 * layout-relevant state change without risking an update loop.
 */

/** Narrowest usable file pane. */
export const MIN_FILE_WIDTH = 200;
/** Narrowest usable right panel. */
export const MIN_RIGHT_WIDTH = 200;
/** ChatPanel's `min-w-[...]` — below this the collapsed toolbar breaks. */
export const MIN_CHAT_WIDTH = 288;

/**
 * Horizontal cost of everything in the row that is neither a pane nor sized by
 * a pane: NavBar 48 + right rail 40 + two 4px resize handles + the right
 * panel's 4px `ml-1` gutter = 100.
 *
 * The gutter is counted even when the right panel is hidden, which makes the
 * fit 4px conservative — it can clamp 4px earlier than strictly necessary.
 * Not worth branching a second constant over.
 */
export const CHROME_WIDTH = 100;

export interface PaneFitInput {
  /** Live viewport width in CSS pixels. */
  windowWidth: number;
  sidebarWidth: number;
  fileWidth: number;
  /**
   * Width the right panel actually occupies. Pass 0 when it is not rendered
   * (collapsed, or no agent selected) — the caller owns that condition because
   * it also owns the JSX that renders the panel.
   */
  rightWidth: number;
}

export interface PaneFitResult {
  fileWidth: number;
  rightWidth: number;
}

/**
 * Shrink the file pane, then the right pane, until the row fits the viewport.
 * Returns the inputs unchanged when there is no overflow.
 */
export function fitPanesToViewport({
  windowWidth,
  sidebarWidth,
  fileWidth,
  rightWidth,
}: PaneFitInput): PaneFitResult {
  const surplus =
    windowWidth - CHROME_WIDTH - MIN_CHAT_WIDTH - sidebarWidth - fileWidth - rightWidth;
  if (surplus >= 0) return { fileWidth, rightWidth };

  const deficit = -surplus;
  const fromFile = Math.min(deficit, Math.max(fileWidth - MIN_FILE_WIDTH, 0));
  const fromRight = Math.min(deficit - fromFile, Math.max(rightWidth - MIN_RIGHT_WIDTH, 0));
  return { fileWidth: fileWidth - fromFile, rightWidth: rightWidth - fromRight };
}
