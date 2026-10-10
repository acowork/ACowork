//! Regression guard for "custom provider dialog can't set the compact
//! model".
//!
//! The custom-provider step of `AddProviderFlow` used to render
//! `showCompactModel={false}` and `handleAddCustom` omitted `compactModel`
//! from the `add_key` payload, so a user adding a custom provider had to
//! save, close, re-open the provider through the edit dialog, and only
//! then pick the summarization model. Adding and editing now offer the
//! same control.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { AddProviderFlow } from "./AddProviderFlow";
import type { ProviderListEntry, ModelInfo } from "../../lib/types";

const DISCOVERED: ModelInfo[] = [
  { id: "acme-large", name: "Acme Large" },
  { id: "acme-flash", name: "Acme Flash" },
];

const mockFetchProviders = vi.fn<[], Promise<ProviderListEntry[]>>(async () => []);
const mockFetchProviderModels = vi.fn<[string], Promise<{ models: unknown[] }>>(
  async () => ({ models: [] }),
);
const mockDiscoverModels = vi.fn<[string, string | undefined], Promise<ModelInfo[]>>(
  async () => DISCOVERED,
);
const mockListKeys = vi.fn<[], Promise<unknown[]>>(async () => []);
const mockInvoke = vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>(
  async (cmd) => {
    if (cmd === "list_keys") return mockListKeys();
    return {};
  },
);

vi.mock("../../lib/gateway-api", () => ({
  fetchProviders: () => mockFetchProviders(),
  fetchProviderModels: (id: string) => mockFetchProviderModels(id),
  discoverModels: (url: string, key?: string) => mockDiscoverModels(url, key),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => mockInvoke(cmd, args),
}));

const noop = () => {};

function findByPlaceholder(placeholder: string): HTMLInputElement {
  return screen.getByPlaceholderText(placeholder) as HTMLInputElement;
}

/** The compact-model <select>. It only renders once ≥1 model is selected,
 *  and its options are the selected model IDs (plus the "use current
 *  model" placeholder). Its <label> is not associated via htmlFor, so it
 *  has no accessible name — the custom step renders exactly one
 *  <select>, which makes `getAllByRole` unambiguous here. */
function findCompactSelect(): HTMLSelectElement {
  const selects = screen.getAllByRole("combobox") as HTMLSelectElement[];
  expect(selects).toHaveLength(1);
  return selects[0];
}

/** Walk the custom step: name → id auto-slug → base URL (blur triggers
 *  discovery) → tick two models → pick the compact one → Save. */
async function fillCustomStep() {
  await act(async () => {
    fireEvent.change(findByPlaceholder("e.g. My GPT Proxy"), {
      target: { value: "Acme" },
    });
  });
  const baseUrl = findByPlaceholder("https://api.example.com/v1");
  await act(async () => {
    fireEvent.change(baseUrl, { target: { value: "https://acme.test/v1" } });
    fireEvent.blur(baseUrl);
  });

  // Model rows render as <label><input type="checkbox">…</label>.
  const boxes = screen.getAllByRole("checkbox") as HTMLInputElement[];
  await act(async () => {
    fireEvent.click(boxes[0]);
    fireEvent.click(boxes[1]);
  });
}

describe("AddProviderFlow custom step — compact model", () => {
  beforeEach(() => {
    mockFetchProviders.mockClear();
    mockFetchProviderModels.mockClear();
    mockDiscoverModels.mockClear();
    mockListKeys.mockClear();
    mockInvoke.mockClear();
  });

  it("saves the picked compact model with the custom provider", async () => {
    const onClose = vi.fn();
    render(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={onClose}
        onSuccess={noop}
      />,
    );

    await fillCustomStep();

    const compact = findCompactSelect();
    expect([...compact.options].map((o) => o.value)).toEqual([
      "",
      "acme-large",
      "acme-flash",
    ]);

    await act(async () => {
      fireEvent.change(compact, { target: { value: "acme-flash" } });
    });

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /^Save$|保存$/ }));
    });

    const addKey = mockInvoke.mock.calls.find(([cmd]) => cmd === "add_key");
    expect(addKey).toBeDefined();
    expect(addKey?.[1]).toMatchObject({
      provider: "custom-acme",
      baseUrl: "https://acme.test/v1",
      models: ["acme-large", "acme-flash"],
      compactModel: "acme-flash",
      custom: true,
    });
    expect(onClose).toHaveBeenCalled();
  });

  it("omits compactModel when the user leaves it on the default", async () => {
    render(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );

    await fillCustomStep();
    expect(findCompactSelect().value).toBe("");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /^Save$|保存$/ }));
    });

    const addKey = mockInvoke.mock.calls.find(([cmd]) => cmd === "add_key");
    expect(addKey?.[1]).toMatchObject({ compactModel: undefined });
  });

  it("clears the compact pick between two consecutive opens", async () => {
    const { rerender } = render(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );
    await fillCustomStep();
    await act(async () => {
      fireEvent.change(findCompactSelect(), { target: { value: "acme-flash" } });
    });

    rerender(
      <AddProviderFlow
        open={false}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );
    rerender(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );

    await fillCustomStep();
    expect(findCompactSelect().value).toBe("");
  });
});
