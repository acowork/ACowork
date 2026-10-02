/**
 * The Users group must carry the same row hairline the Agents group does.
 *
 * The report: agent rows in the sidebar are separated by a hairline, the
 * user rows under them are not, so the two groups read as two different
 * lists stitched together. `UserRow` is the only sidebar row that never
 * got the divider — the agent row and the two other sidebar pickers
 * ([ProjectSidebar](../../views/pm/ProjectSidebar.tsx),
 * [ExtensionsView](../../views/ExtensionsView.tsx)) all draw it by hand.
 *
 * This pins the visual contract: every row but the last one in the group
 * gets the divider, and the last one does not (a trailing rule under the
 * final row would double up with the group's own bottom border).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { UserList } from "./UserList";
import { useAuthStore } from "../../stores/authStore";
import { useUserChatStore } from "../../stores/userChatStore";
import type { Role, UserAccount } from "../../lib/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => ({})),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

function account(role: Role, user_id: string, display_name: string): UserAccount {
  return {
    user_id,
    username: user_id,
    display_name,
    role,
    language: "en",
    timezone: "UTC",
    created_at: "2025-01-01T00:00:00Z",
    updated_at: "2025-01-01T00:00:00Z",
  };
}

const DIVIDER = "after:border-b";

describe("UserList rows carry the agent-list hairline", () => {
  beforeEach(() => {
    const self = account("admin", "u-self", "Alice");
    const peerA = account("user", "u-peer-a", "Bravo");
    const peerB = account("user", "u-peer-b", "Chandra");
    useAuthStore.setState({
      mode: "multi_user",
      status: "logged_in",
      account: self,
      accessToken: "token",
      viewAsUserId: null,
      registrationOpen: false,
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [self, peerA, peerB] }),
        text: async () => "{}",
      })),
    );
    useUserChatStore.setState({ activePeerId: null, activePeerLabel: null, chats: [], messages: [] });
  });

  it("divides every pair of rows but leaves the last row undecorated", async () => {
    await act(async () => {
      render(<UserList />);
    });

    const rows = [screen.getByTestId("user-row-u-self"), screen.getByTestId("user-row-u-peer-a")];

    // `u-peer-b` sorts last (admins first, then display name), so it is the
    // only row that must NOT get a rule.
    for (const row of rows) {
      expect(row.className).toContain(DIVIDER);
    }
    expect(screen.getByTestId("user-row-u-peer-b").className).not.toContain(DIVIDER);
  });

  it("still drops the rule on a single-row group", async () => {
    useAuthStore.setState({ account: account("admin", "u-self", "Alice") });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ accounts: [useAuthStore.getState().account!] }),
        text: async () => "{}",
      })),
    );

    await act(async () => {
      render(<UserList />);
    });

    expect(screen.getByTestId("user-row-u-self").className).not.toContain(DIVIDER);
  });
});
