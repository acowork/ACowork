/**
 * Mode-aware access to the caller's own profile (ADR-076 §决策 3 / 12).
 *
 * The profile API comes in two shapes and the Desktop has to speak both:
 *
 *   - `AUTH_MODE=local` (ADR-076 §决策 12, the historical loopback
 *     deployment): anonymous single-user profiles behind `/api/users`. Reads
 *     answer `{ users, version }` with the "current" user flagged by
 *     `is_active`; writes answer `{ user, version }`.
 *   - `AUTH_MODE=multi_user`: `accounts.json` is the authority. The caller's
 *     own record is `GET /api/auth/me` (a flat `AccountView`), and writes go
 *     to `PUT /api/users/{user_id}` with a flat body. There is no
 *     `is_active` concept — every account is its own — and `GET /api/users`
 *     answers `{ accounts }` (and is admin-only for non-self reads), so the
 *     local-mode `data.users.find(...)` dereferenced `undefined`.
 *
 * Keeping the branch here instead of in each caller lets ProfileTab and
 * OnboardingFlow run against one `BackendUserProfile` shape in both modes.
 */

import { useAuthStore } from "../stores/authStore";
import { getGatewayUrl } from "./config";
import { createUser, fetchActiveUser, updateUser } from "./gateway-api";
import type { BackendUserProfile, UpdateUserRequest, UserAccount } from "./types";

/**
 * Project an account record onto the profile shape the settings UI renders.
 *
 * `is_active` has no `multi_user` counterpart (there is no active-user
 * selector — the caller *is* the user), but the UI reads it, so it is
 * synthesized as always-true.
 */
function accountToProfile(account: UserAccount): BackendUserProfile {
  return {
    user_id: account.user_id,
    display_name: account.display_name,
    language: account.language,
    timezone: account.timezone,
    city: account.city,
    country: account.country,
    occupation: account.occupation,
    communication_style: account.communication_style,
    custom: account.custom,
    created_at: account.created_at,
    updated_at: account.updated_at,
    is_active: true,
    avatar: account.avatar ?? null,
    builtin_avatar: account.builtin_avatar ?? null,
  };
}

/** `GET /api/auth/me` — the caller's own account (ADR-076 §决策 3). */
async function fetchMeAccount(gatewayUrl: string): Promise<UserAccount | null> {
  const token = useAuthStore.getState().accessToken;
  if (!token) return null;
  const resp = await fetch(`${gatewayUrl}/api/auth/me`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!resp.ok) throw new Error(`Failed to fetch account: ${resp.status}`);
  return (await resp.json()) as UserAccount;
}

function isMultiUser(): boolean {
  return useAuthStore.getState().mode === "multi_user";
}

/**
 * The caller's own profile, or `null` when there is none yet (local mode
 * without an active user, or multi_user without a session) — callers render
 * their "no profile yet" state for that.
 */
export async function fetchSelfProfile(
  gatewayUrl = getGatewayUrl(),
): Promise<BackendUserProfile | null> {
  if (isMultiUser()) {
    const account = await fetchMeAccount(gatewayUrl);
    return account ? accountToProfile(account) : null;
  }
  return fetchActiveUser(gatewayUrl);
}

/**
 * Patch the caller's own profile and return the stored record.
 *
 * `multi_user` PUTs the caller's account — deliberately with no `role` field,
 * because the Gateway only honours a role change for admins and a user must
 * never be able to promote themselves. `local` keeps the legacy
 * `PUT /api/users/{active_user_id}` flow, whose response is wrapped in
 * `{ user, version }`.
 */
export async function updateSelfProfile(
  patch: UpdateUserRequest,
  gatewayUrl = getGatewayUrl(),
): Promise<BackendUserProfile> {
  if (isMultiUser()) {
    const account = await fetchMeAccount(gatewayUrl);
    if (!account) throw new Error("Not signed in");
    const resp = await fetch(`${gatewayUrl}/api/users/${account.user_id}`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: resp.statusText }));
      throw new Error(
        (err as { error?: string }).error ?? `Failed to update profile: ${resp.status}`,
      );
    }
    // AccountView, flat — no `{ user, version }` envelope here.
    return accountToProfile((await resp.json()) as UserAccount);
  }

  const active = await fetchActiveUser(gatewayUrl);
  if (!active) throw new Error("No active user profile");
  return updateUser(active.user_id, patch, gatewayUrl);
}

/** The identity the onboarding wizard collects in its last-but-one step. */
export interface OnboardingIdentity {
  display_name: string;
  language: string;
  timezone: string;
  city?: string;
  occupation?: string;
}

/**
 * Persist the identity collected by the onboarding wizard.
 *
 * Under `multi_user` the account already exists — the boot order is
 * login-then-onboarding — so the wizard must *patch* it. The legacy call it
 * replaces issued `POST /api/users`, which created a second account nobody is
 * signed in as: the wizard reported success while the logged-in user's
 * profile stayed empty. `local` keeps the create path, where the wizard is
 * the only writer of the anonymous single-user profile.
 */
export async function saveOnboardedIdentity(
  identity: OnboardingIdentity,
  gatewayUrl = getGatewayUrl(),
): Promise<void> {
  if (isMultiUser()) {
    await updateSelfProfile(identity, gatewayUrl);
    return;
  }
  await createUser(identity, gatewayUrl);
}
