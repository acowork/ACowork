//! Regression guard for the "provider appears in BOTH lists" report.
//!
//! Once a provider has an account it must leave the picker entirely (custom,
//! local AND remote groups): a picker row has no per-row edit/remove, so a
//! leftover copy drifts out of sync with the Configured Providers list and
//! leaves no way to manage it. Unconfigured providers keep their Connect /
//! Add-key button.

import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import i18n from "../../i18n";
import { ProviderPicker } from "./ProviderPicker";
import type { ProviderListEntry, VaultKeyEntry } from "../../lib/types";

const providers: ProviderListEntry[] = [
  { id: "custom-agnes", name: "custom-agnes", model_count: 1, custom: true },
  { id: "custom-fresh", name: "custom-fresh", model_count: 0, custom: true },
  { id: "ollama", name: "Ollama", model_count: 0, local: true },
  { id: "lmstudio", name: "LM Studio", model_count: 0, local: true },
  { id: "openai", name: "OpenAI", model_count: 40 },
  { id: "anthropic", name: "Anthropic", model_count: 12 },
];

// One account for a custom, a local and a remote provider.
const keys: VaultKeyEntry[] = [
  { provider: "custom-agnes", key_preview: "sk-...iAI" },
  { provider: "ollama", key_preview: "(local)" },
  { provider: "openai", key_preview: "sk-...xyz" },
];

describe("ProviderPicker", () => {
  it("hides every provider that already has an account", () => {
    render(
      <ProviderPicker
        providers={providers}
        keys={keys}
        onConnect={vi.fn()}
        onAddCustom={vi.fn()}
      />,
    );

    // Configured → gone from all three groups.
    expect(screen.queryByText("custom-agnes")).toBeNull();
    expect(screen.queryByText("Ollama")).toBeNull();
    expect(screen.queryByText("OpenAI")).toBeNull();

    // Unconfigured → still offered.
    expect(screen.getByText("custom-fresh")).toBeTruthy();
    expect(screen.getByText("LM Studio")).toBeTruthy();
    expect(screen.getByText("Anthropic")).toBeTruthy();

    // Connect (custom + local) and Add-key (remote) buttons, one each.
    expect(
      screen.getAllByRole("button", { name: i18n.t("harness.connect") }),
    ).toHaveLength(2);
    expect(
      screen.getAllByRole("button", { name: i18n.t("harness.addKey") }),
    ).toHaveLength(1);
  });
});
