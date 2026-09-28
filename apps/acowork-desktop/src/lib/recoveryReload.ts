/**
 * The webview-relaunch signal shared by `authStore.logout()` (writer) and
 * `App.tsx` (reader).
 *
 * `authStore` writes the flag, then calls `window.location.reload()`. On the
 * next load `App.tsx` reads it to skip the SplashScreen: the Gateway process
 * is per-machine, not per-account, so an account switch must not re-run
 * `bootGateway` + the /health poll + the 1.5s minimum-splash linger.
 *
 * WHY MODULE SCOPE: the read is deliberately cached once per module load.
 * `App.tsx` used to read `sessionStorage` on every render; the recovery
 * effect clears the flag mid-mount, so React's dev-only React.StrictMode
 * remount (and any store-driven re-render after the effect runs) re-read a
 * deleted flag, fell out of the recovery branch into `gatewayReady=false`,
 * and booted the SplashScreen — whose `ensure_system_agent` ran without a
 * bearer token and 401'd 5/5, dumping the user back on LoginView. Caching
 * here keeps the branch selection stable for the whole webview lifetime.
 */
export const RECOVERY_RELOAD_FLAG = "acowork_recovery_reload";

/** Read once per module load. Never re-read — see the file header. */
export const isRecoveryReload: boolean = (() => {
  try {
    return sessionStorage.getItem(RECOVERY_RELOAD_FLAG) === "1";
  } catch {
    // Non-browser / storage-disabled environment — treat as a normal boot.
    return false;
  }
})();

/** Clear the flag once the recovery branch has handled it. */
export function clearRecoveryReload(): void {
  try {
    sessionStorage.removeItem(RECOVERY_RELOAD_FLAG);
  } catch {
    // ignore
  }
}
