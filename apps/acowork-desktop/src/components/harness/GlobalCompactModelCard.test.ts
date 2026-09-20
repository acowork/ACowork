import { describe, expect, it } from "vitest";
import { buildCompactModelOptions } from "./GlobalCompactModelCard";
import type { VaultKeyEntry } from "../../lib/types";

const key = (
  provider: string,
  models: string[] | undefined,
  defaultModel?: string,
): VaultKeyEntry => ({
  provider,
  key_preview: "***",
  models,
  default_model: defaultModel,
});

describe("buildCompactModelOptions", () => {
  it("returns an empty list when no keys are configured", () => {
    expect(buildCompactModelOptions([])).toEqual([]);
  });

  it("expands one option per (provider, model) pair in input order", () => {
    const out = buildCompactModelOptions([
      key("anthropic", ["claude-3", "claude-2"]),
      key("openai", ["gpt-4o"]),
    ]);
    expect(out.map((o) => o.key)).toEqual([
      "anthropic::claude-3",
      "anthropic::claude-2",
      "openai::gpt-4o",
    ]);
  });

  // Regression: deepseek with 2 accounts used to emit
  // "deepseek::deepseek-flash" twice, which made the Dropdown carry
  // duplicate React keys and triggered a per-render reconcile storm
  // (visible as a flood of long-task warnings on the Harness tab).
  // This picker is on the (provider, model) axis — account count must
  // not multiply the option list.
  it("collapses N accounts of the same provider into one option per model", () => {
    const out = buildCompactModelOptions([
      // First deepseek account
      key("deepseek", ["deepseek-chat", "deepseek-flash"]),
      // Second deepseek account (same provider, same catalog, distinct key)
      key("deepseek", ["deepseek-chat", "deepseek-flash"]),
      // Third account with a partial overlap
      key("deepseek", ["deepseek-flash", "deepseek-reasoner"]),
    ]);
    expect(out.map((o) => o.key)).toEqual([
      "deepseek::deepseek-chat",
      "deepseek::deepseek-flash",
      "deepseek::deepseek-reasoner",
    ]);
  });

  it("preserves order across providers and picks the first-seen model order", () => {
    const out = buildCompactModelOptions([
      key("openai", ["gpt-4o", "gpt-4o-mini"]),
      key("anthropic", ["claude-3"]),
      // Second openai account — its model list is ignored (first
      // account already contributed the canonical ordering).
      key("openai", ["gpt-4o-mini", "gpt-4o"]),
    ]);
    expect(out.map((o) => o.key)).toEqual([
      "openai::gpt-4o",
      "openai::gpt-4o-mini",
      "anthropic::claude-3",
    ]);
  });

  it("falls back to `default_model` when `models` is empty/absent", () => {
    expect(
      buildCompactModelOptions([key("custom", undefined, "llama-3")]).map(
        (o) => o.key,
      ),
    ).toEqual(["custom::llama-3"]);
    expect(buildCompactModelOptions([key("custom", [])])).toEqual([]);
  });
});
