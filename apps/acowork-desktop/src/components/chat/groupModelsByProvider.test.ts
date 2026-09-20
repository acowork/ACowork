import { describe, it, expect } from "vitest";
import { groupModelsByProvider } from "./ChatPanel";

describe("groupModelsByProvider", () => {
  // Regression: vault `list_keys` returned deepseek first, then later
  // (after a key was added) returned it last. Insertion-order grouping
  // produced two different menus, and the second one desynced the
  // account fly-out's cached `position: fixed` coords from the row the
  // user was hovering (mouse couldn't cross the gap).
  it("is stable across input order — providers and models both alphabetical", () => {
    const deepseekFirst = [
      { name: "deepseek-reasoner", provider: "deepseek" },
      { name: "deepseek-chat", provider: "deepseek" },
      { name: "claude-sonnet", provider: "anthropic" },
      { name: "gpt-4o", provider: "openai" },
    ];
    const deepseekLast = [
      { name: "claude-sonnet", provider: "anthropic" },
      { name: "gpt-4o", provider: "openai" },
      { name: "deepseek-chat", provider: "deepseek" },
      { name: "deepseek-reasoner", provider: "deepseek" },
    ];

    const expected: Array<readonly [string, Array<{ name: string; provider: string }>]> = [
      ["anthropic", [{ name: "claude-sonnet", provider: "anthropic" }]],
      ["deepseek", [
        { name: "deepseek-chat", provider: "deepseek" },
        { name: "deepseek-reasoner", provider: "deepseek" },
      ]],
      ["openai", [{ name: "gpt-4o", provider: "openai" }]],
    ];

    expect(groupModelsByProvider(deepseekFirst, "")).toEqual(expected);
    expect(groupModelsByProvider(deepseekLast, "")).toEqual(expected);
  });

  it("filters by case-insensitive substring on name or provider", () => {
    const models = [
      { name: "claude-sonnet", provider: "anthropic" },
      { name: "deepseek-chat", provider: "deepseek" },
      { name: "gpt-4o", provider: "openai" },
    ];

    // Provider substring
    expect(groupModelsByProvider(models, "DEEP").map(([p]) => p)).toEqual(["deepseek"]);
    // Model name substring
    expect(groupModelsByProvider(models, "sonnet").map(([, ms]) => ms.map((m) => m.name))).toEqual([
      ["claude-sonnet"],
    ]);
  });

  it("treats blank query as no filter", () => {
    const models = [
      { name: "b", provider: "z" },
      { name: "a", provider: "y" },
    ];
    expect(groupModelsByProvider(models, "   ")).toEqual([
      ["y", [{ name: "a", provider: "y" }]],
      ["z", [{ name: "b", provider: "z" }]],
    ]);
  });

  it("does not mutate the input array", () => {
    const models = [
      { name: "b", provider: "z" },
      { name: "a", provider: "y" },
    ];
    const snapshot = JSON.stringify(models);
    groupModelsByProvider(models, "");
    expect(JSON.stringify(models)).toBe(snapshot);
  });
});