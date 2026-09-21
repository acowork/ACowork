/**
 * Self-check for the ADR-076 §决策 6 invite affordance:
 *
 * The Gateway allows a logged-in non-admin to create accounts only while
 * `[multi_user].registration_open` is on (and hardcodes the created role to
 * `user`). The sidebar must mirror that exactly — showing the "+" to a
 * non-admin when registration is closed would offer a button that answers 403.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { UserList } from "./UserList";
import { useAuthStore } from "../../stores/authStore";
import type { Role, UserAccount } from "../../lib/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => ({})),
}));

function account(role: Role): UserAccount {
  return {
    user_id: `u-${role}`,
    username: role,
    display_name: role === "admin" ? "Alice" : "Bob",
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

  it("shows the create button to a non-admin while registration is open", async () => {
    signIn("user", true);
    await act(async () => {
      render(<UserList />);
    });
    expect(screen.getByTestId("user-create-button")).toBeTruthy();
  });

  it("hides it from a non-admin while registration is closed", async () => {
    signIn("user", false);
    await act(async () => {
      render(<UserList />);
    });
    expect(screen.queryByTestId("user-create-button")).toBeNull();
  });

  it("always shows it to an admin", async () => {
    signIn("admin", false);
    await act(async () => {
      render(<UserList />);
    });
    expect(screen.getByTestId("user-create-button")).toBeTruthy();
  });
});
