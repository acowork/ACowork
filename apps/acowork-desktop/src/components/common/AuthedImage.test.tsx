/**
 * Self-check for `AuthedImage` (ADR-076 follow-up).
 *
 * The bug this locks down: under `multi_user` a bare `<img src="…/avatar-file">`
 * cannot send the bearer token, so every custom avatar came back 401 and the
 * UI silently showed the builtin icon instead. `AuthedImage` must therefore
 * (a) resolve through `fetch` and render the resulting blob URL, and
 * (b) hand back `fallback` for a falsy src *and* for a failed fetch.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { AuthedImage } from "./AuthedImage";

let originalFetch: typeof globalThis.fetch;
let originalCreate: unknown;
let originalRevoke: unknown;

beforeEach(() => {
  originalFetch = globalThis.fetch;
  originalCreate = URL.createObjectURL;
  originalRevoke = URL.revokeObjectURL;
  // jsdom ships no blob-URL implementation; the component only needs a
  // stable string back and a revoke it can call on unmount.
  URL.createObjectURL = vi.fn(() => "blob:avatar");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  // Unmount while the blob stubs are still installed — the component
  // revokes its object URL in the effect cleanup.
  cleanup();
  globalThis.fetch = originalFetch;
  URL.createObjectURL = originalCreate as typeof URL.createObjectURL;
  URL.revokeObjectURL = originalRevoke as typeof URL.revokeObjectURL;
});

function okResponse(): Response {
  return {
    ok: true,
    status: 200,
    blob: async () => new Blob(["bytes"]),
  } as unknown as Response;
}

describe("AuthedImage", () => {
  it("fetches the url and renders the blob it resolves to", async () => {
    const fetchSpy = vi.fn(async () => okResponse());
    globalThis.fetch = fetchSpy as unknown as typeof globalThis.fetch;

    const { container } = render(
      <AuthedImage src="http://gw.test/api/agents/a/avatar-file?path=assets/avatar-01.png" alt="a" />,
    );

    await waitFor(() => {
      expect(container.querySelector("img")?.getAttribute("src")).toBe("blob:avatar");
    });
    expect(fetchSpy).toHaveBeenCalledWith(
      "http://gw.test/api/agents/a/avatar-file?path=assets/avatar-01.png",
    );
  });

  it("renders the fallback when the fetch is rejected (the 401 path)", async () => {
    globalThis.fetch = vi.fn(async () => ({
      ok: false,
      status: 401,
      blob: async () => new Blob([]),
    })) as unknown as typeof globalThis.fetch;

    const { container, getByText } = render(
      <AuthedImage src="http://gw.test/api/agents/a/avatar-file?path=assets/avatar-01.png" fallback={<span>builtin</span>} />,
    );

    expect(getByText("builtin")).toBeTruthy();
    expect(container.querySelector("img")).toBeNull();
  });

  it("renders the fallback for a falsy src without touching the network", () => {
    const fetchSpy = vi.fn();
    globalThis.fetch = fetchSpy as unknown as typeof globalThis.fetch;

    const { getByText } = render(<AuthedImage src={null} fallback={<span>builtin</span>} />);

    expect(getByText("builtin")).toBeTruthy();
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
