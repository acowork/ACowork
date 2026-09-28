/**
 * Guards the account-switch recovery flag (lib/recoveryReload.ts).
 *
 * Regression: `App.tsx` used to read `sessionStorage` on every render. The
 * recovery effect clears the flag mid-mount, so React.StrictMode's dev-only
 * remount (and store-driven re-renders) re-read a deleted flag → left the
 * recovery branch → `gatewayReady=false` → SplashScreen → `ensure_system_agent`
 * without a bearer token → 5/5 401 → back on LoginView.
 *
 * The contract under test: the flag is read ONCE per module load and never
 * re-read, so clearing it cannot flip a live boot back onto the splash path.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { RECOVERY_RELOAD_FLAG } from "./recoveryReload";

/** Import a fresh module instance, as a webview load would. */
async function loadFresh() {
  vi.resetModules();
  return import("./recoveryReload");
}

describe("recoveryReload", () => {
  beforeEach(() => {
    sessionStorage.clear();
  });

  it("reads true when the flag is present at module load", async () => {
    sessionStorage.setItem(RECOVERY_RELOAD_FLAG, "1");
    const mod = await loadFresh();
    expect(mod.isRecoveryReload).toBe(true);
  });

  it("reads false when the flag is absent at module load", async () => {
    const mod = await loadFresh();
    expect(mod.isRecoveryReload).toBe(false);
  });

  it("keeps the cached value after the flag is cleared mid-mount", async () => {
    sessionStorage.setItem(RECOVERY_RELOAD_FLAG, "1");
    const mod = await loadFresh();
    expect(mod.isRecoveryReload).toBe(true);

    // Simulate App's recovery effect clearing the flag after mount.
    mod.clearRecoveryReload();
    expect(sessionStorage.getItem(RECOVERY_RELOAD_FLAG)).toBeNull();

    // The already-loaded module must NOT change its answer (this is the bug:
    // a re-read here returns false and drops the boot back to the splash).
    expect(mod.isRecoveryReload).toBe(true);
  });

  it("clearRecoveryReload is a no-op when the flag was never set", async () => {
    const mod = await loadFresh();
    expect(() => mod.clearRecoveryReload()).not.toThrow();
    expect(sessionStorage.getItem(RECOVERY_RELOAD_FLAG)).toBeNull();
  });
});
