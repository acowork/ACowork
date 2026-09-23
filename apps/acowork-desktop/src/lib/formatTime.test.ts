/**
 * formatBubbleTime tests.
 *
 * Three contracts must hold:
 *
 *   - Falsy / invalid input collapses to "" so the bubble component can
 *     skip rendering the timestamp span entirely on legacy entries.
 *   - Valid timestamps render a non-empty string.
 *   - All six fields survive, zero-padded, with a 4-digit year. Field
 *     ORDER is deliberately not asserted: `formatBubbleTime` formats with
 *     the runtime locale, so "2026/08/30 12:34:56" (zh-CN) and
 *     "08/30/2026, 12:34:56" (en-US) are both correct.
 */
import { describe, it, expect } from "vitest";
import { formatBubbleTime } from "./formatTime";

describe("formatBubbleTime", () => {
  it("returns empty string for falsy input", () => {
    expect(formatBubbleTime(undefined)).toBe("");
    expect(formatBubbleTime(null)).toBe("");
    expect(formatBubbleTime(0)).toBe("");
  });

  it("returns empty string for invalid timestamps", () => {
    expect(formatBubbleTime(Number.NaN)).toBe("");
    expect(formatBubbleTime(Number.POSITIVE_INFINITY)).toBe("");
    expect(formatBubbleTime(Number.NEGATIVE_INFINITY)).toBe("");
  });

  it("renders a non-empty string for a valid timestamp", () => {
    const ms = new Date(2026, 7, 30, 12, 34, 56).getTime();
    const out = formatBubbleTime(ms);
    expect(out).not.toBe("");
    expect(out.length).toBeGreaterThan(8);
  });

  it("keeps all six fields, zero-padded and locale-order-independent", () => {
    // Local-time constructor so the assertion is independent of the runner's
    // timezone (jsdom picks up Asia/Shanghai here, CI may use UTC).
    const ms = new Date(2026, 7, 30, 12, 34, 56).getTime();
    const out = formatBubbleTime(ms);
    const groups = out.match(/\d+/g) ?? [];
    expect(groups).toHaveLength(6); // YYYY + MM + DD + HH + MM + SS
    expect(groups).toContain("2026"); // 4-digit year, not "26"
    expect(groups).toContain("08"); // month zero-padded, not "8"
    expect(groups).toContain("30"); // day zero-padded
    expect(groups.filter((g) => g.length === 2)).toHaveLength(5);
  });
});