/**
 * Explicit boot wiring for the token module's rotation transport
 * (ADR-076 §决策 3).
 *
 * `lib/session/tokens.ts` stays free of store and config imports by taking its
 * transport through `configure()`. Wiring it from a store's module-load side
 * effect made every *other* consumer — the Tauri auth bridge above all —
 * silently depend on that store having been imported first, a load-order
 * coupling nothing in the type system or the import graph enforced.
 *
 * Installing it here, once, from `main.tsx` before the interceptor and the
 * bridge, puts the dependency in the boot call graph where it is visible:
 * whoever installs a consumer must first install the transport.
 */

import { getGatewayUrl } from "../config";
import { logoutRequest, refreshRequest } from "../auth-api";
import { configure } from "./tokens";

/** Supply the token module its rotation transport. Call once, at boot. */
export function installTokenTransport(): void {
  configure({
    refresh: async (refreshToken) => refreshRequest(getGatewayUrl(), refreshToken),
    logout: async (refreshToken) => {
      await logoutRequest(getGatewayUrl(), refreshToken);
    },
  });
}
