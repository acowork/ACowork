/**
 * Unit tests for the sidebar Users group shape (ADR-076 §决策 7).
 *
 * `partitionAccounts` is the pure part of the group rendering — pin its
 * two contracts: an empty list yields no group, and admins sort first
 * with a stable name order inside each role.
 */
import { describe, it, expect } from "vitest";
import { partitionAccounts } from "./partitionAccounts";
import type { UserAccount } from "../../lib/types";

const account = (over: Partial<UserAccount>): UserAccount => ({
  user_id: "u1",
  username: "u1",
  display_name: "User",
  role: "user",
  language: "en",
  timezone: "UTC",
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
  ...over,
});

describe("partitionAccounts", () => {
  it("returns no group for an empty list", () => {
    expect(partitionAccounts([])).toEqual([]);
  });

  it("sorts admins first, then by display name", () => {
    const groups = partitionAccounts([
      account({ user_id: "b", display_name: "Bob" }),
      account({ user_id: "root", display_name: "Root", role: "admin" }),
      account({ user_id: "a", display_name: "Alice" }),
    ]);

    expect(groups).toHaveLength(1);
    expect(groups[0].accounts.map((a) => a.user_id)).toEqual(["root", "a", "b"]);
  });

  it("does not lose accounts", () => {
    const list = [
      account({ user_id: "1" }),
      account({ user_id: "2", role: "admin" }),
      account({ user_id: "3" }),
    ];
    const groups = partitionAccounts(list);
    expect(groups[0].accounts).toHaveLength(list.length);
  });
});
