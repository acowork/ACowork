/**
 * useEscapeClose — bind Escape to a dismiss callback while `open`.
 *
 * Exists because the six modal dialogs (PermissionDialog,
 * AgentDetailDialog, ConfirmDialog, CloneDialog, CreateWizard,
 * PublishWizard) each inlined the same effect, and every copy listed the
 * close callback in its dependency array:
 *
 *     useEffect(() => {
 *       if (!open) return;
 *       const handler = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
 *       window.addEventListener("keydown", handler);
 *       return () => window.removeEventListener("keydown", handler);
 *     }, [open, onClose]);
 *
 * `onClose` is almost always an inline arrow at the call site
 * (`onClose={() => setPermTarget(null)}`), so it is a NEW function on
 * every parent render. Any unrelated parent re-render — an MQTT
 * `session_status_changed`, a re-published inventory, the 3 s gateway
 * death-watch probe — therefore tore down and re-added the listener.
 *
 * In the three dialogs that ALSO focus a button in the same effect that
 * was not harmless. The re-run re-executed `closeRef.current?.focus()`,
 * yanking focus out of whatever the user was typing in, and a native
 * `<select>` popup closes when its element loses focus, so the owner
 * picker's dropdown collapsed seconds after it was opened. That is the
 * whole bug: nothing was refreshing, focus was simply being stolen on a
 * timer driven by unrelated store churn.
 *
 * The callback is only ever *called* here, never *subscribed* to, so it
 * does not belong in the dependency array. This hook keeps it in a ref
 * and subscribes exactly once per open/closed transition; callers may pass
 * an unstable function with no effect on the listener lifecycle.
 */
import { useEffect, useRef } from "react";

export function useEscapeClose(
  /** Whether the modal is currently mounted/open. Drives subscription. */
  open: boolean,
  /** Invoked on Escape. Read from a ref, so identity is irrelevant. */
  onEscape: () => void,
  /**
   * Extra gate read at KEYDOWN time, not subscription time — e.g. the
   * wizards' `!busy`, which must not have to re-subscribe every time a
   * button flips its spinner.
   */
  enabled: boolean = true,
): void {
  const latest = useRef({ onEscape, enabled });
  // Written during render (not in an effect) so the handler always sees
  // this commit's callback and `enabled` — an effect would leave one frame
  // where Escape hits a stale closure.
  latest.current = { onEscape, enabled };

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      const { onEscape: cb, enabled: on } = latest.current;
      if (e.key === "Escape" && on) cb();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open]);
}