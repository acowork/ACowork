/**
 * Self-check for the chat-view overflow ladder (`panelClamp.ts`).
 *
 * Bug being fixed: with the file pane dragged to its maximum and the right
 * panel collapsed, the row's rigid panes summed to less than the viewport.
 * Clicking the right-nav to open the panel added `rightWidth` pixels that
 * nothing absorbed — ChatPanel is already pinned at its 288px minimum and the
 * other three panes are `shrink-0` — so the row overflowed and the leftmost
 * pane (the agent list) was pushed out of the window.
 *
 * Properties verified:
 *   1. No overflow → every width returned untouched (the clamp must not
 *      pre-emptively shrink a pane the user sized by hand).
 *   2. Overflow → the file pane pays first, down to its minimum.
 *   3. File pane already at its minimum → the right pane pays, and only as
 *      much as the deficit requires.
 *   4. Both at their minimum → nothing changes (honest clipping, the agent
 *      list is never the sacrifice).
 *   5. Idempotent — feeding a result back in is a no-op, which is what makes
 *      it safe to run from a React effect.
 *   6. ChatPanel maxed / file pane min is NOT an overflow case: `flex-1`
 *      already absorbed it, so the clamp stays out of the way.
 */
import { describe, expect, it } from "vitest";
import {
  CHROME_WIDTH,
  MIN_CHAT_WIDTH,
  MIN_FILE_WIDTH,
  MIN_RIGHT_WIDTH,
  fitPanesToViewport,
} from "./panelClamp";

/** Rigid-panes total plus chrome must never exceed the viewport. */
const occupied = (sidebar: number, file: number, right: number) =>
  CHROME_WIDTH + MIN_CHAT_WIDTH + sidebar + file + right;

describe("fitPanesToViewport", () => {
  it("leaves a row that already fits completely alone", () => {
    const r = fitPanesToViewport({ windowWidth: 1920, sidebarWidth: 240, fileWidth: 450, rightWidth: 340 });
    expect(r).toEqual({ fileWidth: 450, rightWidth: 340 });
  });

  it("takes the reported overflow out of the file pane, not the agent list", () => {
    // The repro: file pane dragged to its 900px max, right panel then opened.
    const r = fitPanesToViewport({ windowWidth: 1920, sidebarWidth: 240, fileWidth: 900, rightWidth: 600 });
    expect(r.fileWidth).toBeLessThan(900);
    expect(r.rightWidth).toBe(600); // the panel the user just opened keeps its width
    expect(occupied(240, r.fileWidth, r.rightWidth)).toBeLessThanOrEqual(1920);
  });

  it("does not clamp a maxed file pane while the right panel is hidden", () => {
    // Same widths, right panel not rendered (caller passes 0).
    const r = fitPanesToViewport({ windowWidth: 1920, sidebarWidth: 240, fileWidth: 900, rightWidth: 0 });
    expect(r).toEqual({ fileWidth: 900, rightWidth: 0 });
  });

  it("falls through to the right pane once the file pane has bottomed out", () => {
    const r = fitPanesToViewport({ windowWidth: 1400, sidebarWidth: 400, fileWidth: MIN_FILE_WIDTH, rightWidth: 600 });
    expect(r.fileWidth).toBe(MIN_FILE_WIDTH);
    expect(r.rightWidth).toBeLessThan(600);
    expect(r.rightWidth).toBeGreaterThanOrEqual(MIN_RIGHT_WIDTH);
    expect(occupied(400, r.fileWidth, r.rightWidth)).toBeLessThanOrEqual(1400);
  });

  it("shrinks the right pane only as far as the deficit requires", () => {
    // Row needs 12px more than the window has: the right pane gives up exactly
    // 12px, not its whole 400px of slack.
    const windowWidth = occupied(240, MIN_FILE_WIDTH, 600) - 12;
    const r = fitPanesToViewport({ windowWidth, sidebarWidth: 240, fileWidth: MIN_FILE_WIDTH, rightWidth: 600 });
    expect(r.fileWidth).toBe(MIN_FILE_WIDTH);
    expect(r.rightWidth).toBe(600 - 12);
  });

  it("gives up instead of eating the agent list when both payers are spent", () => {
    // 1000px cannot hold 400 + 200 + 200 + 288 + 100. The row clips; the
    // agent list keeps its width (it has no vote in this ladder).
    const r = fitPanesToViewport({ windowWidth: 1000, sidebarWidth: 400, fileWidth: MIN_FILE_WIDTH, rightWidth: MIN_RIGHT_WIDTH });
    expect(r).toEqual({ fileWidth: MIN_FILE_WIDTH, rightWidth: MIN_RIGHT_WIDTH });
  });

  it("spends the file pane before the agent list even when the list is wider", () => {
    // Agent list at its 400px max is the widest pane here; the 300px file pane
    // still pays first, and the right pane keeps its full width.
    const r = fitPanesToViewport({ windowWidth: 1300, sidebarWidth: 400, fileWidth: 300, rightWidth: 300 });
    expect(r.fileWidth).toBe(212);
    expect(r.rightWidth).toBe(300);
  });

  it("is idempotent", () => {
    const first = fitPanesToViewport({ windowWidth: 1500, sidebarWidth: 240, fileWidth: 900, rightWidth: 600 });
    const second = fitPanesToViewport({ windowWidth: 1500, sidebarWidth: 240, ...first });
    expect(second).toEqual(first);
  });

  it("survives a window narrower than the minimum row without going negative", () => {
    const r = fitPanesToViewport({ windowWidth: 600, sidebarWidth: 400, fileWidth: 900, rightWidth: 600 });
    expect(r.fileWidth).toBe(MIN_FILE_WIDTH);
    expect(r.rightWidth).toBe(MIN_RIGHT_WIDTH);
  });
});
