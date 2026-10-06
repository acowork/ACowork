import { useState, useEffect, useRef, useCallback } from "react";
import { useTranslation } from "../../i18n/useTranslation";
import { cn } from "../../lib/utils";
import { getGatewayUrl } from "../../lib/config";
import { useAuthStore } from "../../stores/authStore";
import { fetchAccounts, fetchDirectory } from "../../lib/auth-api";
import type { DirectoryUser } from "../../lib/types";
import {
  fetchAgentPermissions,
  fetchNodePermissions,
  patchAgentVisibility,
  patchAgentGuests,
  patchNodeVisibility,
  patchNodeGuests,
  patchAgentOwner,
  patchNodeOwner,
  type ResourcePermissions,
} from "../../lib/gateway-api";
import { ErrorBox } from "../common/ErrorBox";
import { Switch } from "../common/Switch";

/** What the dialog edits: an agent instance or a node. */
export interface PermissionTarget {
  kind: "agent" | "node";
  /** Canonical instance key / node id (ADR-073 — never a package id). */
  id: string;
  /** Display name for the dialog header. */
  name: string;
}

interface PermissionDialogProps {
  open: boolean;
  target: PermissionTarget | null;
  onClose: () => void;
}

/**
 * ADR-087 permissions dialog — owner / guests / visibility editor for a
 * single agent or node. Rendered in the app-wide dialog chrome (backdrop +
 * max-w-md card, same zones as AgentDetailDialog).
 *
 * Gating contract (mirrors the Gateway middleware):
 * - The entry points that open this dialog are hidden unless the list
 *   payload said `can_manage` — this component assumes a manageable caller.
 * - `can_attribute` (owner ∨ admin ∨ local) decides whether the form is
 *   editable; a manage-guest sees the same data read-only (D9 R1).
 * - Owner transfer is admin-only and intentionally not exposed here (the
 *   PATCH endpoint exists for the future admin surface).
 */
export function PermissionDialog({ open, target, onClose }: PermissionDialogProps) {
  const { t } = useTranslation();
  const [perms, setPerms] = useState<ResourcePermissions | null>(null);
  const [users, setUsers] = useState<DirectoryUser[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** In-flight admin claim (ADR-087 D7) — separate from `saving` so the
   *  claim button can show its own spinner without disabling the form. */
  const [claiming, setClaiming] = useState(false);
  // Local draft state — committed on Save.
  const [visibility, setVisibility] = useState<string>("private");
  const [guests, setGuests] = useState<Set<string>>(new Set());
  const closeRef = useRef<HTMLButtonElement>(null);

  const load = useCallback(async () => {
    if (!open || !target) return;
    setLoading(true);
    setError(null);
    try {
      const p =
        target.kind === "agent"
          ? await fetchAgentPermissions(target.id)
          : await fetchNodePermissions(target.id);
      setPerms(p);
      setVisibility(p.visibility);
      setGuests(new Set(p.guests));
    } catch (e) {
      setError(String(e));
      setPerms(null);
    } finally {
      setLoading(false);
    }
    // Best-effort name source. A guest list is an ALREADY-AUTHORIZED
    // roster, not a contact picker, so it must name every id on it —
    // including accounts that are disabled or the caller themself, both
    // of which `/api/users/directory` filters out by design. Rendering a
    // raw UUID there makes the roster unreadable and the one entry the
    // owner most needs to identify (a revoked/departed account) the one
    // they cannot read.
    //
    // `GET /api/users` is admin-only and covers the full set (disabled
    // included), so an admin — the role that can edit this list at all —
    // reads the complete roster from it. Non-admins can only view
    // (`can_attribute: false`) and fall back to the directory; an
    // unresolvable id there degrades to the id, as before.
    //
    // Same admin/non-admin split as `UserList.refetch`.
    const token = useAuthStore.getState().accessToken;
    const isAdmin = useAuthStore.getState().account?.role === "admin";
    if (token) {
      try {
        const rows = isAdmin
          ? (await fetchAccounts(getGatewayUrl(), token)).map((a) => ({
              user_id: a.user_id,
              username: a.username,
              display_name: a.display_name,
              avatar: a.avatar ?? null,
              builtin_avatar: a.builtin_avatar ?? null,
            }))
          : await fetchDirectory(getGatewayUrl(), token);
        setUsers(rows);
      } catch {
        setUsers([]);
      }
    } else {
      setUsers([]);
    }
  }, [open, target]);

  useEffect(() => {
    void load();
  }, [load]);

  // Focus close on open; Escape to close (same affordance as the detail dialog).
  useEffect(() => {
    if (!open) return;
    closeRef.current?.focus();
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onClose]);

  if (!open || !target) return null;

  const editable = perms?.can_attribute === true;
  // Agent vocabulary is shared/private, node's is public/private (ADR-087 D6).
  const onValue = target.kind === "agent" ? "shared" : "public";
  const onLabel = t(
    target.kind === "agent" ? "permissionDialog.visibilityShared" : "permissionDialog.visibilityPublic",
  );
  const offLabel = t("permissionDialog.visibilityPrivate");

  const dirty =
    perms !== null &&
    (visibility !== perms.visibility ||
      guests.size !== perms.guests.length ||
      perms.guests.some((g) => !guests.has(g)));

  const handleSave = async () => {
    if (!perms || !dirty) return;
    setSaving(true);
    setSaveError(null);
    try {
      if (visibility !== perms.visibility) {
        const fn = target.kind === "agent" ? patchAgentVisibility : patchNodeVisibility;
        await fn(target.id, visibility);
      }
      if (guests.size !== perms.guests.length || perms.guests.some((g) => !guests.has(g))) {
        const fn = target.kind === "agent" ? patchAgentGuests : patchNodeGuests;
        await fn(target.id, Array.from(guests));
      }
      await load();
    } catch (e) {
      // `HttpApiError.message` is already the localized denial copy;
      // `String(e)` would re-prefix it with "Error:" and dump raw bodies.
      setSaveError(e instanceof Error ? e.message : String(e));
      // Snap the draft back to the last known server truth. Otherwise the
      // switch keeps showing a value that was never persisted, and the
      // user reads a rejected write as a successful one. The error box
      // above carries the reason (e.g. 409: claim an owner first).
      setVisibility(perms.visibility);
      setGuests(new Set(perms.guests));
    } finally {
      setSaving(false);
    }
  };

  const toggleGuest = (userId: string) => {
    setGuests((prev) => {
      const next = new Set(prev);
      if (next.has(userId)) next.delete(userId);
      else next.add(userId);
      return next;
    });
  };

  /**
   * Claim this resource for the signed-in admin (ADR-087 D7).
   *
   * An ownerless resource is admin-only and cannot be published, so
   * every agent/node that predates ADR-087 opens in a state where the
   * visibility switch is disabled. This is the recovery path.
   *
   * The claim deliberately does NOT also set visibility. An ownerless
   * row is private by construction, and defaulting the claim to
   * "…and publish it to every logged-in user" would silently widen
   * who can reach the agent — an authorization decision that belongs to
   * the admin, not to this button. Claim, then flip the switch.
   */
  const handleClaim = async () => {
    if (!perms || !target) return;
    const me = useAuthStore.getState().account;
    if (!me) return;
    setClaiming(true);
    setSaveError(null);
    try {
      const fn = target.kind === "agent" ? patchAgentOwner : patchNodeOwner;
      await fn(target.id, me.user_id);
      await load();
    } catch (e) {
      // `HttpApiError.message` is already the localized denial copy;
      // `String(e)` would re-prefix it with "Error:" and dump raw bodies.
      setSaveError(e instanceof Error ? e.message : String(e));
    } finally {
      setClaiming(false);
    }
  };

  // Guests the name source could not resolve (a soft-deleted account, or
  // a Gateway too old for either listing). Kept visible by id so the
  // roster never silently drops an entry on save.
  const dirIds = new Set(users.map((u) => u.user_id));
  const orphanGuests = perms ? perms.guests.filter((g) => !dirIds.has(g)) : [];
  // Owner name. `authStore.account` is the caller's own record, which
  // covers the one case the directory can never answer: an admin who
  // claims a resource becomes its owner, and `/api/users/directory`
  // deliberately excludes the caller (it is a contact picker). It is
  // checked first so the common "owner is me" case resolves even on the
  // non-admin listing path. An id that resolves to neither is a
  // soft-deleted account — the id is shown rather than a blank, so the
  // row never looks ownerless and invite a second claim.
  const me = useAuthStore.getState().account;
  const ownerRec =
    (me && perms?.owner_user_id === me.user_id
      ? { user_id: me.user_id, display_name: me.display_name }
      : undefined) ?? users.find((u) => u.user_id === perms?.owner_user_id);
  const ownerName = perms?.owner_user_id
    ? ownerRec?.display_name ?? perms.owner_user_id
    : null;
  // An ownerless resource is admin-only and cannot be published (ADR-087
  // D6/D7). The Gateway ships `can_set_visibility` so the switch is
  // disabled rather than offered-then-rejected by a 409. Only an admin
  // may claim, since `PATCH .../owner` is the Admin tier.
  const isOwnerless = perms !== null && perms.owner_user_id === null;
  const canClaim = isOwnerless && me?.role === "admin";
  // `can_attribute` gates the form; `can_set_visibility` gates the one
  // write the store would silently normalise away. Absent on a pre-087
  // Gateway — fall back so old Gateways keep working.
  const canSetVisibility = perms?.can_set_visibility ?? perms?.can_attribute ?? true;
  // An ownerless row cannot hold a published value, so the switch is
  // frozen outright: no direction is writable, because the row is
  // private by construction and turning it on is exactly the write the
  // store would undo. `editable` (owner ∨ admin) still gates the rest
  // of the form.
  const visibilityLocked = !editable || (isOwnerless && !canSetVisibility);
  const visibilityHint =
    isOwnerless && !canSetVisibility
      ? t("permissionDialog.needsOwnerHint", { pub: onLabel })
      : visibility === onValue
        ? t("permissionDialog.visibilityOnHint")
        : t("permissionDialog.visibilityOffHint");

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-modal-overlay" onClick={onClose} />

      {/* Dialog */}
      <div className="relative z-10 w-full max-w-md rounded-md border border-border-outer bg-modal-surface shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border-divider px-5 py-3 min-h-[var(--ui-dialog-zone-h)]">
          <h3 className="truncate text-sm font-semibold">
            {t("permissionDialog.title")}
            <span className="ml-1.5 font-normal text-text-tertiary">{target.name}</span>
          </h3>
          <button
            ref={closeRef}
            onClick={onClose}
            className="text-text-tertiary hover:text-zinc-600 dark:hover:text-zinc-300"
            aria-label={t("permissionDialog.ariaLabelClose")}
          >
            ✕
          </button>
        </div>

        {/* Body */}
        <div className="max-h-[60vh] space-y-4 overflow-y-auto px-5 py-4 text-xs">
          {loading && !perms && (
            <div className="flex items-center justify-center py-8">
              <div className="h-5 w-5 animate-spin rounded-full border-2 border-zinc-300 border-t-zinc-600 dark:border-zinc-600 dark:border-t-zinc-300" />
            </div>
          )}

          {error && (
            <ErrorBox message={`${t("permissionDialog.loadFailed")}: ${error}`} onClose={() => setError(null)} />
          )}

          {perms && !loading && (
            <>
              {/* Owner (read-only — transfer is admin-only, ADR-087 D7) */}
              <div className="flex items-center justify-between gap-2">
                <span className="text-text-tertiary">{t("permissionDialog.owner")}</span>
                {/* `title` so a name too long for the row (or the raw id
                    fallback for a deleted account) is still readable on
                    hover instead of being silently truncated. `data-owner`
                    marks this cell as THE owner — the guest roster also
                    lists the owner, so the name alone is ambiguous. */}
                <span
                  data-owner
                  className="truncate text-text-secondary"
                  title={ownerName ?? undefined}
                >
                  {ownerName ?? t("permissionDialog.ownerUnassigned")}
                </span>
              </div>

              {/* Ownerless ⇒ admin-only and unpublishable (ADR-087 D6/D7).
                  Without this the only way forward is a hand-written
                  PATCH, so the visibility switch below is a dead end. */}
              {canClaim && (
                <div className="flex flex-col gap-2 rounded-md border border-border-divider bg-surface-secondary/50 px-3 py-2.5">
                  <div className="text-11 text-text-secondary">
                    {t("permissionDialog.claimOwnerHint", {
                      kind: target.kind === "agent" ? t("permissionDialog.agent") : t("permissionDialog.node"),
                      pub: onLabel,
                    })}
                  </div>
                  <button
                    onClick={() => void handleClaim()}
                    disabled={claiming}
                    className="self-start rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-white transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    {claiming ? t("permissionDialog.claiming") : t("permissionDialog.claimOwner")}
                  </button>
                </div>
              )}

              {/* Visibility toggle */}
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0">
                  <div className="text-text-secondary">{t("permissionDialog.visibility")}</div>
                  <div className="text-10 text-text-tertiary">
                    {visibilityHint}
                  </div>
                </div>
                {/* Shared Switch — do NOT hand-roll a track+thumb toggle here.
                    The previous inline <button> positioned its thumb with only
                    `absolute top-0.5` + `translate-x-*`, no `left-*`. `left: auto`
                    fell back to the thumb's *static* position, which inside a
                    <button> is the inline box under the UA `text-align: center`
                    (Tailwind preflight does not reset it) — so the dot rendered
                    10px right of the leading edge when off, and at 26+16=42px
                    against a 36px track when on, i.e. outside the switch.
                    <Switch> pins `left-0.5` + `top-1/2 -translate-y-1/2`, so the
                    static-position fallback can never apply. */}
                <Switch
                  checked={visibility === onValue}
                  onChange={(v) => setVisibility(v ? onValue : "private")}
                  disabled={visibilityLocked}
                  aria-label={t("permissionDialog.visibility")}
                />
              </div>

              {/* State label, directly under the switch.
                  This used to render BOTH names side by side ("私有  共享")
                  as a static legend. Two problems: the spans were not
                  aligned to the switch's own two positions, so nothing
                  connected a name to a state, and both names looked
                  equally "current" while the track already shows which
                  one is. A single label naming the state that is
                  actually in effect is what the reader wants. */}
              <div className="flex justify-end text-10 text-text-tertiary">
                <span>{visibility === onValue ? onLabel : offLabel}</span>
              </div>

              {/* Guests */}
              <div>
                <div className="mb-1 text-text-secondary">{t("permissionDialog.guests")}</div>
                <div className="text-10 mb-2 text-text-tertiary">{t("permissionDialog.guestsHint")}</div>
                {users.length === 0 && orphanGuests.length === 0 ? (
                  <div className="rounded-md border border-border-divider px-3 py-2 text-text-tertiary">
                    {t("permissionDialog.noUsers")}
                  </div>
                ) : (
                  <div className="max-h-44 space-y-0.5 overflow-y-auto rounded-md border border-border-divider px-2 py-1.5">
                    {users.map((u) => (
                      <label
                        key={u.user_id}
                        className={cn(
                          "flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 hover:bg-nav-item-hover",
                          !editable && "cursor-not-allowed opacity-60 hover:bg-transparent",
                        )}
                      >
                        <input
                          type="checkbox"
                          className="accent-[var(--color-accent)]"
                          checked={guests.has(u.user_id)}
                          disabled={!editable}
                          onChange={() => toggleGuest(u.user_id)}
                        />
                        {/* `title` so a long display name survives the
                            `truncate` — the row is the only place a guest
                            is named, so a clipped name is a lost one. */}
                        <span className="truncate" title={u.display_name || u.username}>
                          {u.display_name || u.username}
                        </span>
                        <span className="ml-auto truncate text-10 text-text-tertiary">{u.username}</span>                      </label>
                    ))}
                    {orphanGuests.map((g) => (
                      <label
                        key={g}
                        className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 hover:bg-nav-item-hover"
                        title={t("permissionDialog.guestUnavailable")}
                      >
                        <input
                          type="checkbox"
                          className="accent-[var(--color-accent)]"
                          checked
                          disabled={!editable}
                          onChange={() => toggleGuest(g)}
                        />
                        <span className="truncate italic text-text-tertiary">{g}</span>
                      </label>
                    ))}
                  </div>
                )}
              </div>

              {!editable && (
                <div className="rounded-md border border-border-divider bg-nav-item-hover px-3 py-2 text-10 text-text-tertiary">
                  {t("permissionDialog.readOnly")}
                </div>
              )}
            </>
          )}

          {saveError && (
            <ErrorBox message={`${t("permissionDialog.saveFailed")}: ${saveError}`} onClose={() => setSaveError(null)} />
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 border-t border-border-divider px-5 min-h-[var(--ui-dialog-zone-h)]">
          <button
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-xs font-medium text-text-secondary hover:bg-zinc-100 dark:hover:bg-zinc-700"
          >
            {t("permissionDialog.close")}
          </button>
          {editable && (
            <button
              onClick={() => void handleSave()}
              disabled={!dirty || saving}
              className="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-white transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {t("permissionDialog.save")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
