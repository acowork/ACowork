//! Regression guard for the "stale form values on re-open" report.
//!
//! `AddProviderFlow` is mounted unconditionally in `HarnessPage`; when
//! `open` flips to `false` the visual shell unmounts (`if (!open) return
//! null`) but the hook state lives on. Without an explicit wipe on each
//! `open === true` transition, fields typed in a previous session leak
//! into the next one — e.g. the second "Add Custom Provider" pre-fills
//! the first one, or only clears when the user navigates away to another
//! tab and back.
//!
//! The fix lives in the `useEffect([open])` block at the top of
//! `AddProviderFlow.tsx` — every form field is reset there before the
//! initial step is applied. This test pins that behavior down so it
//! can't silently regress.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { AddProviderFlow } from "./AddProviderFlow";
import type { ProviderListEntry } from "../../lib/types";

const mockFetchProviders = vi.fn<[], Promise<ProviderListEntry[]>>(
  async () => [],
);
const mockFetchProviderModels = vi.fn<[string], Promise<{ models: unknown[] }>>(
  async () => ({ models: [] }),
);
const mockListKeys = vi.fn<[], Promise<unknown[]>>(async () => []);

vi.mock("../../lib/gateway-api", () => ({
  fetchProviders: () => mockFetchProviders(),
  fetchProviderModels: (id: string) => mockFetchProviderModels(id),
  discoverModels: vi.fn(async () => []),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    if (cmd === "list_keys") return mockListKeys();
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  },
}));

const noop = () => {};

/** Find the custom-step Provider Name <input>. The Custom step renders
 *  exactly one input with placeholder "e.g. My GPT Proxy" — querying by
 *  placeholder is the most stable selector since labels are i18n-bound. */
function findNameInput(): HTMLInputElement {
  return screen.getByPlaceholderText("e.g. My GPT Proxy") as HTMLInputElement;
}
function findBaseUrlInput(): HTMLInputElement {
  return screen.getByPlaceholderText("https://api.example.com/v1") as HTMLInputElement;
}

describe("AddProviderFlow reset on re-open", () => {
  beforeEach(() => {
    mockFetchProviders.mockClear();
    mockFetchProviderModels.mockClear();
    mockListKeys.mockClear();
  });

  it("clears custom-step fields between two consecutive opens", async () => {
    // First open: type a name and base URL, then close.
    const { rerender } = render(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );

    await act(async () => {
      fireEvent.change(findNameInput(), { target: { value: "Acme" } });
      fireEvent.change(findBaseUrlInput(), { target: { value: "https://acme.test/v1" } });
    });

    expect(findNameInput().value).toBe("Acme");
    expect(findBaseUrlInput().value).toBe("https://acme.test/v1");

    // Close — component returns null but hook state survives.
    rerender(
      <AddProviderFlow
        open={false}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );

    // Re-open — fields must be empty.
    rerender(
      <AddProviderFlow
        open={true}
        initialStep="custom"
        onClose={noop}
        onSuccess={noop}
      />,
    );

    expect(findNameInput().value).toBe("");
    expect(findBaseUrlInput().value).toBe("");
  });
});