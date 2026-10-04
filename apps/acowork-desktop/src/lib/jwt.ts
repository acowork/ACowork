/**
 * Unverified readers for JWT-shaped access tokens (ADR-076).
 *
 * The signature is deliberately NOT verified here — these helpers only
 * decide *when to ask the Gateway for a rotation*. The Gateway verifies
 * signature, expiry and family binding on every request and every MQTT
 * CONNECT, so a mis-read payload can only make us rotate slightly early;
 * it can never authenticate anyone. Mirrors `access_token_sub` /
 * `access_token_exp` in `src-tauri/src/commands/chat_mqtt.rs`, which apply
 * the same rule on the Rust side.
 */

/** Decoded payload of a JWT-shaped token, or `null` when unreadable. */
export function decodeJwtPayload(token: string): Record<string, unknown> | null {
  const parts = token.split(".");
  if (parts.length < 2) return null;
  try {
    // base64url → base64, then re-pad: JWT segments carry no `=`.
    const b64 = parts[1].replace(/-/g, "+").replace(/_/g, "/");
    const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    // `atob` yields a latin1 string; route through bytes so a non-ASCII
    // claim (e.g. a display name) survives the decode.
    const bytes = Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
    const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
    return typeof parsed === "object" && parsed !== null
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

/**
 * `exp` claim in epoch **milliseconds**, or `null` when the token has no
 * readable numeric `exp`. JWT `exp` is in seconds (RFC 7519 §4.1.4).
 */
export function jwtExpMs(token: string): number | null {
  const exp = decodeJwtPayload(token)?.exp;
  return typeof exp === "number" && Number.isFinite(exp) ? exp * 1000 : null;
}
