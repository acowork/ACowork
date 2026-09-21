/**
 * Sidebar "Users" folding group (ADR-076 §决策 5 / 6 / 7).
 *
 * Rendered below the agent list in [AgentList.tsx](../agent-list/AgentList.tsx).
 * Only appears under `AUTH_MODE=multi_user` with a live session — under
 * `local` the whole account system is a no-op (§决策 12), so the group is
 * not shown.
 *
 * Admin tokens list every account (`GET /api/users`) and can manage them
 * from a row context menu (reset password / role / disable) plus a
 * header "+" create modal. Ordinary users only see themselves, matching
 * the Gateway's own authorization. Selecting a row (admin) switches
 * session listing into that user's read-only view via `?as_user=` (§决策 5).
 */

import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { ChevronRight, KeyRound, MessageSquare, Plus, ShieldCheck, ShieldOff, UserX, X } from "lucide-react";
import { useAuthStore } from "../../stores/authStore";
import { useLayoutStore } from "../../stores/layoutStore";
import { useUserChatStore } from "../../stores/userChatStore";
import { useAgentStore } from "../../stores/agentStore";
import { useTranslation } from "../../i18n/useTranslation";
import { getGatewayUrl } from "../../lib/config";
import {
  AuthApiError,
  disableAccount,
  fetchAccounts,
  resetPassword,
  setRole,
  type CreateAccountResult,
} from "../../lib/auth-api";
import type { UserAccount } from "../../lib/types";
import { cn } from "../../lib/utils";
import { UserAvatar } from "../common/UserAvatar";
import { ConfirmDialog } from "../common/ConfirmDialog";
import {
  ContextMenu,
  useContextMenu,
  type ContextMenuItem,
} from "../common/ContextMenu";
import { CreateAccountModal } from "../account/CreateAccountModal";
import { InviteTokenModal } from "../account/InviteTokenModal";
import { partitionAccounts } from "./partitionAccounts";

export function UserList() {
  const { t } = useTranslation();
  const mode = useAuthStore((s) => s.mode);
  const status = useAuthStore((s) => s.status);
  const self = useAuthStore((s) => s.account);
  const accessToken = useAuthStore((s) => s.accessToken);
  const viewAsUserId = useAuthStore((s) => s.viewAsUserId);
  const registrationOpen = useAuthStore((s) => s.registrationOpen);
  const selectedAgentId = useAgentStore((s) => s.selectedAgentId);

  const [collapsed, setCollapsed] = useState(true);
  const [accounts, setAccounts] = useState<UserAccount[]>([]);
  const [loadFailed, setLoadFailed] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [invite, setInvite] = useState<{ token: string; username?: string } | null>(null);
  const [disableTarget, setDisableTarget] = useState<UserAccount | null>(null);

  const menu = useContextMenu<UserAccount>();

  const loggedIn = mode === "multi_user" && status === "logged_in";
  const isAdmin = self?.role === "admin";
  // §决策 6: a non-admin may create accounts only while self-registration is
  // open (the Gateway hardcodes the created role to `user`). Admins always may.
  const canInvite = isAdmin || registrationOpen;

  const reload = useCallback(async () => {
    const token = useAuthStore.getState().accessToken;
    try {
      setAccounts(await fetchAccounts(getGatewayUrl(), token ?? ""));
      setLoadFailed(false);
    } catch {
      setLoadFailed(true);
    }
  }, []);

  useEffect(() => {
    if (!loggedIn) return;
    if (!isAdmin) {
      setAccounts(self ? [self] : []);
      return;
    }
    void reload();
  }, [loggedIn, isAdmin, self, accessToken, reload]);

  if (!loggedIn) return null;

  const rows = partitionAccounts(accounts)[0]?.accounts ?? [];
  const viewingName =
    viewAsUserId != null
      ? rows.find((a) => a.user_id === viewAsUserId)?.display_name ?? viewAsUserId
      : null;

  const select = (userId: string) => {
    const next = userId === viewAsUserId || userId === self?.user_id ? null : userId;
    useAuthStore.getState().setViewAsUserId(next);
    if (selectedAgentId) void useAgentStore.getState().fetchSessions(selectedAgentId);
  };

  const run = async (fn: () => Promise<void>) => {
    setActionError(null);
    try {
      await fn();
      await reload();
    } catch (err) {
      setActionError(err instanceof AuthApiError ? err.message : t("account.actionFailed"));
    }
  };

  const menuItems = (account: UserAccount): ContextMenuItem<UserAccount>[] => {
    const isSelf = account.user_id === self?.user_id;
    const items: ContextMenuItem<UserAccount>[] = [];
    if (!isSelf && !account.disabled_at) {
      // Start a conversation (ADR-076 §决策 8): open the thread and switch the
      // main area to the inbox. A disabled account has no inbox to deliver to,
      // and messaging yourself is not a conversation.
      items.push({
        key: "message",
        icon: <MessageSquare size={14} />,
        label: t("messages.openChat"),
        onClick: () => {
          void useUserChatStore
            .getState()
            .openChat(account.user_id, account.display_name);
          useLayoutStore.getState().requestNavView("users");
        },
      });
    }
    items.push(
      {
        key: "reset-password",
        icon: <KeyRound size={14} />,
        label: t("account.resetPassword"),
        onClick: () =>
          run(async () => {
            const token = useAuthStore.getState().accessToken ?? "";
            const inviteToken = await resetPassword(getGatewayUrl(), token, account.user_id);
            setInvite({ token: inviteToken, username: account.display_name });
          }),
      },
      account.role === "user"
        ? {
            key: "promote",
            icon: <ShieldCheck size={14} />,
            label: t("account.makeAdmin"),
            onClick: () =>
              run(async () => {
                const token = useAuthStore.getState().accessToken ?? "";
                await setRole(getGatewayUrl(), token, account.user_id, "admin");
              }),
          }
        : {
            key: "demote",
            icon: <ShieldOff size={14} />,
            label: t("account.makeUser"),
            // Demoting yourself can strand the deployment without an admin.
            disabled: isSelf,
            onClick: () =>
              run(async () => {
                const token = useAuthStore.getState().accessToken ?? "";
                await setRole(getGatewayUrl(), token, account.user_id, "user");
              }),
          },
    );
    if (!isSelf) {
      items.push({
        key: "disable",
        icon: <UserX size={14} />,
        label: t("account.disableAccount"),
        variant: "danger",
        dividerBefore: true,
        onClick: () => setDisableTarget(account),
      });
    }
    return items;
  };

  return (
    <div data-testid="user-list">
      <div className="flex h-6 items-center border-y border-nav-divider/40 dark:border-zinc-600/40">
        <button
          type="button"
          onClick={() => setCollapsed((c) => !c)}
          aria-expanded={!collapsed}
          aria-label={t("userList.title")}
          data-testid="user-group-header"
          className={cn(
            "flex h-6 min-w-0 flex-1 items-center gap-1.5 px-3 text-left",
            "text-[10px] font-medium uppercase tracking-wide text-text-tertiary",
            "transition-colors duration-150 hover:text-zinc-600 dark:hover:text-zinc-300",
          )}
        >
          <ChevronRight
            className={cn(
              "h-3 w-3 shrink-0 transition-transform duration-150",
              !collapsed && "rotate-90",
            )}
          />
          <span className="truncate">{t("userList.title")}</span>
          <span className="ml-auto text-[10px] font-normal opacity-60">{rows.length}</span>
        </button>
        {canInvite && (
          <button
            type="button"
            onClick={() => setCreateOpen(true)}
            aria-label={t("account.createAccount")}
            title={t("account.createAccount")}
            data-testid="user-create-button"
            className="mr-2 flex h-5 w-5 shrink-0 items-center justify-center rounded text-text-tertiary hover:bg-nav-item-hover"
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        )}
      </div>

      {!collapsed && (
        <>
          {viewingName && (
            <div className="flex items-center gap-1 px-3 py-1 text-[10px] text-text-tertiary">
              <span className="truncate">{t("userList.viewingAs", { name: viewingName })}</span>
              <button
                type="button"
                onClick={() => select(viewAsUserId!)}
                aria-label={t("userList.clearView")}
                title={t("userList.clearView")}
                className="ml-auto rounded p-0.5 hover:bg-nav-item-hover"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          )}

          {actionError && (
            <div role="alert" className="px-3 py-1 text-[10px] text-red-500">
              {actionError}
            </div>
          )}

          {rows.map((account) => {
            const active = account.user_id === viewAsUserId;
            return (
              <div
                key={account.user_id}
                role={isAdmin ? "button" : undefined}
                onClick={isAdmin ? () => select(account.user_id) : undefined}
                onContextMenu={
                  isAdmin ? (e: MouseEvent) => menu.openAt(e, account) : undefined
                }
                data-testid={`user-row-${account.user_id}`}
                title={isAdmin ? t("userList.viewAsUser") : account.display_name}
                className={cn(
                  "flex items-center gap-2 rounded-md px-3 py-1.5 transition-colors duration-150",
                  isAdmin && "cursor-pointer hover:bg-nav-item-hover",
                  active && "bg-[var(--color-accent)]/15",
                )}
              >
                <UserAvatar
                  displayName={account.display_name}
                  avatarUrl={account.avatar ?? null}
                  builtinAvatarId={account.builtin_avatar ?? null}
                  size={20}
                  className="shrink-0"
                />
                <span className="truncate text-xs text-text-secondary">
                  {account.display_name}
                </span>
                <span className="ml-auto text-[10px] text-text-tertiary">
                  {account.role === "admin" ? t("userList.roleAdmin") : t("userList.roleUser")}
                </span>
              </div>
            );
          })}

          {rows.length === 0 && (
            <div className="px-3 py-2 text-[11px] text-text-tertiary">
              {loadFailed ? t("userList.loadFailed") : t("userList.empty")}
            </div>
          )}
        </>
      )}

      {isAdmin && (
        <ContextMenu
          isOpen={menu.isOpen}
          menuProps={menu.menuProps}
          payload={menu.payload}
          selectionAtOpen={menu.selectionAtOpen}
          onClose={menu.close}
          items={menu.payload ? menuItems(menu.payload) : []}
        />
      )}

      <CreateAccountModal
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onCreated={(result: CreateAccountResult) => {
          // `reload` hits the admin-only account list; a non-admin only ever
          // sees their own row, so there is nothing to refresh for them.
          if (isAdmin) void reload();
          if (result.invite_token) {
            setInvite({ token: result.invite_token, username: result.account.display_name });
          }
        }}
      />

      <InviteTokenModal
        open={invite !== null}
        token={invite?.token ?? ""}
        username={invite?.username}
        onClose={() => setInvite(null)}
      />

      <ConfirmDialog
        open={disableTarget !== null}
        title={t("account.disableTitle")}
        message={t("account.disableConfirm", { name: disableTarget?.display_name ?? "" })}
        confirmLabel={t("account.disableAccount")}
        destructive
        onConfirm={() => {
          const target = disableTarget;
          setDisableTarget(null);
          if (target) {
            void run(async () => {
              const token = useAuthStore.getState().accessToken ?? "";
              await disableAccount(getGatewayUrl(), token, target.user_id);
            });
          }
        }}
        onCancel={() => setDisableTarget(null)}
      />
    </div>
  );
}
