/**
 * Shared avatar helpers.
 *
 * - `resolveAgentAvatarUrl(agentId, version?)` builds the full Gateway URL that
 *   serves the agent's packaged avatar (from manifest.avatar) as image bytes.
 *   The optional `version` (manifest.version) is appended as `?v=...` to bust
 *   the browser/WebView HTTP cache when the package is re-installed.
 *   Returns `null` if the URL cannot be built.
 *
 * - `normalizeBuiltinAvatarId(value)` parses a user-authored `builtin_avatar`
 *   value from manifest.toml — accepts either "icon-05" (canonical) or bare
 *   numeric forms "5" / "05" — and returns a canonical "icon-XX" string when
 *   the value matches a bundled icon. Returns `null` for unknown values so
 *   the caller can fall back to a random pick.
 *
 * - `pickRandomBuiltinIconId()` returns a uniformly-random builtin icon ID
 *   from `BUILTIN_ICON_IDS`. Returns `null` if no builtin icons are bundled
 *   (the caller should fall back to a non-icon renderer).
 *
 * - `pickDeterministicBuiltinIconId(seed)` returns a stable random builtin
 *   icon ID for a given string seed. Used to avoid a flash-of-gradient when
 *   an agent has no profile icon and no packaged avatar: the first render
 *   shows the same icon that the install hook will eventually persist.
 */

import { BUILTIN_ICONS, BUILTIN_ICON_IDS } from "./builtinIcons";
import { getGatewayUrl } from "./config";
import type {
  AvatarAssetsResponse,
  AvatarConfigResponse,
  UpdateAvatarConfigRequest,
} from "./types";

// ── ADR-017: Avatar config API helpers ──────────────────────────────────

/** Build URL for a custom avatar file in the agent's install directory. */
export function resolveAgentAvatarFileUrl(agentId: string, relativePath: string): string {
  return `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/avatar-file?path=${encodeURIComponent(relativePath)}`;
}

/** Fetch the list of custom avatar assets from the agent's install directory. */
export async function fetchAvatarAssets(agentId: string): Promise<AvatarAssetsResponse> {
  const resp = await fetch(
    `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/manifest/avatar-assets`,
  );
  if (!resp.ok) throw new Error(`Failed to fetch avatar assets: ${resp.status}`);
  return resp.json();
}

/** Fetch the effective avatar config (works when agent is stopped). */
export async function fetchAvatarConfig(agentId: string): Promise<AvatarConfigResponse> {
  const resp = await fetch(
    `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/avatar-config`,
  );
  if (!resp.ok) throw new Error(`Failed to fetch avatar config: ${resp.status}`);
  return resp.json();
}

/** Update the avatar config (works when agent is stopped). */
export async function updateAvatarConfig(
  agentId: string,
  req: UpdateAvatarConfigRequest,
): Promise<AvatarConfigResponse> {
  const resp = await fetch(
    `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/avatar-config`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(req),
    },
  );
  if (!resp.ok) throw new Error(`Failed to update avatar config: ${resp.status}`);
  return resp.json();
}

/** Delete a custom avatar file from the agent's install directory. */
export async function deleteAvatarFile(agentId: string, relativePath: string): Promise<void> {
  const resp = await fetch(
    `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/avatar-file?path=${encodeURIComponent(relativePath)}`,
    { method: "DELETE" },
  );
  if (!resp.ok) throw new Error(`Failed to delete avatar file: ${resp.status}`);
}

/**
 * Build the Gateway URL that serves the agent's packaged avatar (if any).
 *
 * Pass `version` (manifest.version) to append `?v=<version>`. This is the
 * cache-busting key: when the agent is re-installed with a new version, the
 * URL changes and the browser drops the stale cached image. The Gateway
 * itself emits a long-lived `Cache-Control: public, max-age=31536000,
 * immutable` header so old, superseded URLs remain cacheable for the
 * lifetime of the URL.
 */
export function resolveAgentAvatarUrl(
  agentId: string,
  version?: string | null,
): string | null {
  if (!agentId) return null;
  try {
    const base = `${getGatewayUrl()}/api/agents/${encodeURIComponent(agentId)}/avatar`;
    return version ? `${base}?v=${encodeURIComponent(version)}` : base;
  } catch {
    return null;
  }
}

/**
 * Normalise a manifest `builtin_avatar` value to the canonical "icon-XX" form
 * and validate it against the bundled icon set. Returns the canonical ID
 * (e.g. "icon-05") when the value matches a bundled icon, otherwise `null`.
 *
 * Accepted inputs:
 * - "icon-05", "ICON-5" — canonical/case-insensitive form
 * - "5", "05"          — bare numeric form (1-99)
 *
 * Anything else (empty string, non-numeric, out of range, typo) returns
 * `null` so the caller can fall back to a random icon.
 */
export function normalizeBuiltinAvatarId(value: string | null | undefined): string | null {
  if (!value) return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  const lower = trimmed.toLowerCase();
  // Try "icon-NN" first (canonical form)
  if (lower.startsWith("icon-")) {
    const num = lower.slice("icon-".length);
    if (/^\d{1,2}$/.test(num)) {
      const candidate = `icon-${num.padStart(2, "0")}`;
      if (BUILTIN_ICONS[candidate]) return candidate;
    }
    return null;
  }
  // Then bare numeric 1-99
  if (/^\d{1,2}$/.test(lower)) {
    const candidate = `icon-${lower.padStart(2, "0")}`;
    if (BUILTIN_ICONS[candidate]) return candidate;
  }
  return null;
}

/** Pick a uniformly random builtin icon ID, or null if none are bundled. */
export function pickRandomBuiltinIconId(): string | null {
  if (BUILTIN_ICON_IDS.length === 0) return null;
  const idx = Math.floor(Math.random() * BUILTIN_ICON_IDS.length);
  return BUILTIN_ICON_IDS[idx] ?? null;
}

/** Pick a stable builtin icon ID for the given seed string. */
export function pickDeterministicBuiltinIconId(seed: string): string | null {
  if (BUILTIN_ICON_IDS.length === 0 || !seed) return null;
  let hash = 0;
  for (let i = 0; i < seed.length; i++) {
    hash = (seed.charCodeAt(i) + (hash << 5) - hash) | 0;
  }
  const idx = Math.abs(hash) % BUILTIN_ICON_IDS.length;
  return BUILTIN_ICON_IDS[idx] ?? null;
}

// ── User Avatar API helpers ─────────────────────────────────────────────

/** Response from GET /api/user/avatar-config */
export interface UserAvatarConfig {
  avatar: string | null;
  builtin_avatar: string | null;
}

/** Request body for PUT /api/user/avatar-config */
export interface UpdateUserAvatarConfigRequest {
  avatar?: string | null;
  builtin_avatar?: string | null;
}

/** Build URL for serving a user avatar file. */
export function resolveUserAvatarFileUrl(relativePath: string): string {
  return `${getGatewayUrl()}/api/user/avatar-file?path=${encodeURIComponent(relativePath)}`;
}

/** Fetch the active user's avatar config. */
export async function fetchUserAvatarConfig(): Promise<UserAvatarConfig> {
  const resp = await fetch(`${getGatewayUrl()}/api/user/avatar-config`);
  if (!resp.ok) throw new Error(`Failed to fetch user avatar config: ${resp.status}`);
  return resp.json();
}

/** Update the active user's avatar config. */
export async function updateUserAvatarConfig(
  req: UpdateUserAvatarConfigRequest,
): Promise<UserAvatarConfig> {
  const resp = await fetch(`${getGatewayUrl()}/api/user/avatar-config`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  if (!resp.ok) throw new Error(`Failed to update user avatar config: ${resp.status}`);
  return resp.json();
}

/** Fetch the list of custom avatar assets. */
export async function fetchUserAvatarAssets(): Promise<AvatarAssetsResponse> {
  const resp = await fetch(`${getGatewayUrl()}/api/user/avatar-assets`);
  if (!resp.ok) throw new Error(`Failed to fetch user avatar assets: ${resp.status}`);
  return resp.json();
}

/** Delete a custom user avatar file. */
export async function deleteUserAvatarFile(relativePath: string): Promise<void> {
  const resp = await fetch(
    `${getGatewayUrl()}/api/user/avatar-file?path=${encodeURIComponent(relativePath)}`,
    { method: "DELETE" },
  );
  if (!resp.ok) throw new Error(`Failed to delete user avatar file: ${resp.status}`);
}
