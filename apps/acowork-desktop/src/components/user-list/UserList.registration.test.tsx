/**
 * Self-check for the click-on-row → openChat wiring that ships user↔user
 * inbox threading straight from the sidebar group (no separate "Messages"
 * view).
 *
 * The ADR-076 §决策 6 invite affordance used to live on a banner "+" inside
 * UserList; it now lives in the agent-list bottom "+" menu (`canInviteUser`
 * in [AgentList.tsx](../agent-list/AgentList.tsx)), which mirrors this file's
 * old `canInvite` rule. The visibility tests are gone from here because the
 * DOM node they asserted against was removed; the gating logic itself is
 * unchanged.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { UserList } from "./UserList";
import { useAuthStore } from "../../stores/authStore";
import { useUserChatStore } from "../../stores/userChatStore";
import { useAgentStore } from "../../stores/agentStore";
import type { Role, UserAccount } from "../../lib/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => ({})),
}));

function account(role: Role, user_id?: string, display_name?: string): UserAccount {
  return {
    user_id: user_id ?? `u-${role}`,
    username: role,
    display_name: display_name ?? (role === "admin" ? "Alice" : "Bob"),
    role,
    language: "en",
    timezone: "UTC",
    created_at: "2025-01-01T00:00:00Z",
    updated_at: "2025-01-01T00:00:00Z",
  };
}

function signIn(role: Role, registrationOpen: boolean) {
  useAuthStore.setState({
    mode: "multi_user",
    status: "logged_in",
    account: account(role),
    accessToken: "token",
    viewAsUserId: null,
    registrationOpen,
  });
}

describe("UserList invite affordance (ADR-076 §决策 6)", () => {
  beforeEach(() => {
    // `reload` (admin path) and the account store hit the Gateway; jsdom has
    // no server, so answer with an empty account list.
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [] }),
        text: async () => "{}",
      })),
    );
    useAuthStore.setState({ mode: "unknown", status: "unknown", account: null });
  });

  it("clicking another user's row opens an inbox thread (self row is inert)", async () => {
    // Admin sees both themselves and a peer — the fetchAccounts mock returns
    // an empty list, so install the second account on the store directly.
    const peer = account("user", "u-peer", "Charlie");
    signIn("admin", false);
    // The fetch mock is the global default from beforeEach (empty list).
    // Use a per-test override that lists both the admin and the peer.
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [useAuthStore.getState().account!, peer] }),
        text: async () => "{}",
      })),
    );
    useUserChatStore.setState({
      activePeerId: null,
      activePeerLabel: null,
      chats: [],
      messages: [],
    });
    await act(async () => {
      render(<UserList />);
    });
    // Expand the group so rows render.
    await act(async () => {
      fireEvent.click(screen.getByTestId("user-group-header"));
    });
    const selfRow = screen.getByTestId(`user-row-${useAuthStore.getState().account!.user_id}`);
    const peerRow = screen.getByTestId(`user-row-${peer.user_id}`);
    // Clicking yourself is a no-op (you can't DM yourself).
    await act(async () => {
      fireEvent.click(selfRow);
    });
    expect(useUserChatStore.getState().activePeerId).toBeNull();
    // Clicking another user opens the thread.
    await act(async () => {
      fireEvent.click(peerRow);
    });
    expect(useUserChatStore.getState().activePeerId).toBe(peer.user_id);
    expect(useUserChatStore.getState().activePeerLabel).toBe(peer.display_name);
  });

  it("opening a peer thread clears the active agent so the inbox panel can render", async () => {
    // Regression: AppLayout renders <ChatPanel /> whenever `selectedAgentId`
    // is set and only falls through to <InboxPanel /> when it is null. If
    // `openThread` only sets `activePeerId` and leaves `selectedAgentId`
    // intact, opening a 1:1 thread while an agent is selected silently
    // does nothing visible — the user reports "点 Nicholas 用户右键菜单,
    // 发消息给用户, 没任何反应" while the store did the work underneath.
    const peer = account("user", "u-peer", "Charlie");
    signIn("admin", false);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [useAuthStore.getState().account!, peer] }),
        text: async () => "{}",
      })),
    );
    // Pretend an agent session is already open — this is the precondition
    // that makes the regression visible.
    useAgentStore.setState({ selectedAgentId: "u-existing-agent" });
    useUserChatStore.setState({
      activePeerId: null,
      activePeerLabel: null,
      chats: [],
      messages: [],
    });
    await act(async () => {
      render(<UserList />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("user-group-header"));
    });
    const peerRow = screen.getByTestId(`user-row-${peer.user_id}`);
    await act(async () => {
      fireEvent.click(peerRow);
    });
    // Both stores must end up consistent: no agent selected, peer thread
    // active. The agent clear is what lets AppLayout fall through to the
    // InboxPanel render.
    expect(useAgentStore.getState().selectedAgentId).toBeNull();
    expect(useUserChatStore.getState().activePeerId).toBe(peer.user_id);
  });

  it("a non-admin sees other users from /api/users/directory and can open a chat", async () => {
    // Regression for the contact-picker fix: a non-admin used to see only
    // themselves in the sidebar, with no way to start a 1:1 thread.
    // /api/users/directory (ADR-076 ��决策 8) is the non-admin endpoint.
    const peer = account("user", "u-peer", "Charlie");
    signIn("user", false);
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        if (String(url).endsWith("/api/users/directory")) {
          return {
            ok: true,
            status: 200,
            json: async () => ({ users: [peer] }),
            text: async () => "{}",
          };
        }
        // Defensive: the non-admin path should never hit /api/users.
        return {
          ok: true,
          status: 200,
          json: async () => ({ accounts: [] }),
          text: async () => "{}",
        };
      }),
    );
    useUserChatStore.setState({
      activePeerId: null,
      activePeerLabel: null,
      chats: [],
      messages: [],
    });
    await act(async () => {
      render(<UserList />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("user-group-header"));
    });
    // Directory excludes the caller themselves, so the peer row is the
    // only one rendered — and is clickable to open an inbox thread.
    const peerRow = screen.getByTestId(`user-row-${peer.user_id}`);
    await act(async () => {
      fireEvent.click(peerRow);
    });
    expect(useUserChatStore.getState().activePeerId).toBe(peer.user_id);
  });

  it("falls back to role label only when no chat preview exists", async () => {
    // Regression for the operator-precedence trap:
    //   preview = last_message_preview ?? role === "admin" ? "admin" : "user"
    // parsed as (preview ?? roleAdminFlag) ? "admin" : "user", which made
    // any non-empty preview string flip every row to "管理员". The fix
    // parents the role fallback so it only fires when `??` falls through.
    const peer = account("user", "u-peer", "Charlie");
    signIn("admin", false);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [useAuthStore.getState().account!, peer] }),
        text: async () => "{}",
      })),
    );

    // Case A: no chat exists with the peer → fallback fires → "user" label.
    useUserChatStore.setState({
      activePeerId: null,
      activePeerLabel: null,
      chats: [],
      messages: [],
    });
    await act(async () => {
      render(<UserList />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("user-group-header"));
    });
    const peerRowA = screen.getByTestId(`user-row-${peer.user_id}`);
    // The default-locale i18n resolves `userList.roleUser` to "user" and
    // `userList.roleAdmin` to "admin" — the lowercase labels match the role
    // intent and are unlikely to collide with normal preview content.
    expect(peerRowA.textContent).toContain("user");
    expect(peerRowA.textContent).not.toContain("admin");
  });

  it("uses the chat preview instead of the role fallback when one exists", async () => {
    // Companion case: a chat row's preview must win over the role label.
    // Pre-fix, the ternary sat outside the `??`, so any truthy preview
    // selected "admin" regardless of the actual role.
    const peer = account("user", "u-peer-2", "Dana");
    signIn("admin", false);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [useAuthStore.getState().account!, peer] }),
        text: async () => "{}",
      })),
    );
    useUserChatStore.setState({
      activePeerId: null,
      activePeerLabel: null,
      messages: [],
      chats: [
        {
          chat_id: "x__y",
          peer_user_id: peer.user_id,
          peer_display_name: peer.display_name,
          last_active_at: 0,
          last_message_preview: "Hello from peer",
          unread_count: 0,
        },
      ],
    });
    await act(async () => {
      render(<UserList />);
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("user-group-header"));
    });
    const peerRow = screen.getByTestId(`user-row-${peer.user_id}`);
    expect(peerRow.textContent).toContain("Hello from peer");
    expect(peerRow.textContent).not.toContain("admin");
  });
});
