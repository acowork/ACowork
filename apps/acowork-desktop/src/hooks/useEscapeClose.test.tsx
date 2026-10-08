/**
 * useEscapeClose — the listener must outlive an unstable `onEscape`.
 *
 * The defect this pins: every dialog inlined the Escape effect with
 * `[open, onClose]` deps. The call sites pass an INLINE arrow
 * (`onClose={() => setPermTarget(null)}`), so the callback is a new
 * function on every parent render and the effect re-subscribed each time.
 * Where the same effect also ran `closeRef.current?.focus()`, that stole
 * focus seconds after the user typed or opened a `<select>` — the popup
 * closes on blur, so the dropdown vanished on its own.
 *
 * These assert the contract the hook exists to provide: `onEscape`
 * identity must NOT influence the subscription. Only `open` may.
 */
import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";
import { useEscapeClose } from "./useEscapeClose";

/** Mounts the hook with a fresh `onEscape` identity on every render, which
 *  is what an inline arrow at the call site does. */
function Harness({ open, onEscape, enabled }: {
  open: boolean;
  onEscape: () => void;
  enabled?: boolean;
}) {
  useEscapeClose(open, () => onEscape(), enabled);
  return null;
}

function pressEscape() {
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
}

describe("useEscapeClose", () => {
  it("keeps working after the parent re-renders with a new callback identity", () => {
    const onEscape = vi.fn();
    const { rerender } = render(<Harness open onEscape={onEscape} />);

    // Three "unrelated store tick" re-renders — each hands the hook a
    // brand-new function, exactly like an inline `onClose={() => ...}`.
    rerender(<Harness open onEscape={onEscape} />);
    rerender(<Harness open onEscape={onEscape} />);
    rerender(<Harness open onEscape={onEscape} />);

    pressEscape();
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("sees the LATEST callback, not the one from first render", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = render(<Harness open onEscape={first} />);
    rerender(<Harness open onEscape={second} />);

    pressEscape();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("reads `enabled` at keydown time, not at subscribe time", () => {
    const onEscape = vi.fn();
    const { rerender } = render(<Harness open onEscape={onEscape} enabled={false} />);

    pressEscape();
    expect(onEscape).not.toHaveBeenCalled();

    // Flipping the gate re-renders (a wizard's `busy` clearing) but must
    // not require a re-subscription to take effect.
    rerender(<Harness open onEscape={onEscape} enabled={true} />);
    pressEscape();
    expect(onEscape).toHaveBeenCalledTimes(1);
  });

  it("does not subscribe while closed, and unsubscribes when it closes", () => {
    const onEscape = vi.fn();
    const { rerender } = render(<Harness open={false} onEscape={onEscape} />);

    pressEscape();
    expect(onEscape).not.toHaveBeenCalled();

    rerender(<Harness open onEscape={onEscape} />);
    rerender(<Harness open={false} onEscape={onEscape} />);
    pressEscape();
    expect(onEscape).not.toHaveBeenCalled();
  });

  it("ignores keys other than Escape", () => {
    const onEscape = vi.fn();
    render(<Harness open onEscape={onEscape} />);

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    expect(onEscape).not.toHaveBeenCalled();
  });
});