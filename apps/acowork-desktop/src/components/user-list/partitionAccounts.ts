/**
 * ADR-076 §决策 7: pure helper that shapes the account list for the
 * sidebar "Users" group. Mirrors [partitionAgentsByNode](partitionAgentsByNode.ts)
 * in spirit — kept side-effect-free so it is trivially unit-testable.
 *
 * Unlike agents, accounts are NOT partitioned across nodes: a user is a
 * Gateway-wide identity, orthogonal to the node dimension (one admin can
 * manage agents on many nodes). So there is exactly one group; the
 * helper's job is the deterministic ordering (admins first, then by
 * display name) the UI relies on.
 */

import type { UserAccount } from "../../lib/types";

export interface AccountGroup {
  accounts: UserAccount[];
}

export function partitionAccounts(accounts: UserAccount[]): AccountGroup[] {
  if (accounts.length === 0) return [];
  const sorted = [...accounts].sort((a, b) => {
    if (a.role !== b.role) return a.role === "admin" ? -1 : 1;
    return a.display_name.localeCompare(b.display_name);
  });
  return [{ accounts: sorted }];
}
