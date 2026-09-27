import { describe, expect, it } from "vitest";
import type { ProviderAccount } from "../../lib/types";
import { resolveSessionProviderAlias } from "./RightPanel";

const accounts: Record<string, ProviderAccount[]> = {
  // Multi-account provider — two distinct apikeys for the same vendor.
  openai: [
    { accountId: "acct-openai-work", alias: "work", preview: "…abcd" },
    { accountId: "acct-openai-personal", alias: "personal", preview: "…wxyz" },
  ],
  // Single-account provider — should never surface an alias row.
  anthropic: [{ accountId: "acct-anthropic-1", alias: "main", preview: "…efgh" }],
};

describe("resolveSessionProviderAlias", () => {
  it("returns the alias when the session picked an account on a multi-account provider", () => {
    expect(
      resolveSessionProviderAlias("openai", "acct-openai-personal", accounts),
    ).toBe("personal");
  });

  it("returns null for a single-account provider (alias row stays hidden)", () => {
    expect(resolveSessionProviderAlias("anthropic", "acct-anthropic-1", accounts)).toBeNull();
  });

  it("returns null when the session has not picked an account yet", () => {
    expect(resolveSessionProviderAlias("openai", null, accounts)).toBeNull();
  });

  it("returns null when no provider is set on the session", () => {
    expect(resolveSessionProviderAlias(null, "acct-openai-work", accounts)).toBeNull();
  });

  it("returns null when the picked account_id is not in the vault (deleted mid-session)", () => {
    // Operator removed the key from the vault after the session
    // started — the row must degrade silently rather than surface a
    // stale alias.
    expect(resolveSessionProviderAlias("openai", "acct-deleted", accounts)).toBeNull();
  });

  it("returns null when the provider is not in providerAccounts at all", () => {
    expect(resolveSessionProviderAlias("minimax", "acct-x", accounts)).toBeNull();
  });
});