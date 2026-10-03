//! Regression guard for the MCP install UX report.
//!
//! The report: clicking Install popped a modal with no progress, the OK
//! button dismissed it while the install kept running, the row in the list
//! looked untouched, and a second click re-popped the modal. Minutes later
//! a result box appeared claiming success.
//!
//! Two things broke that loop: the busy state now lives on the row (button
//! is gone, spinner + stage + elapsed are shown, no dialog), and the
//! terminal result renders inline so a failure is visible where the button
//! used to be — turned into "Retry".

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent, within } from "@testing-library/react";
import i18n from "../../i18n";
import { useMcpStore } from "../../stores/mcpStore";
import { McpTab } from "./HarnessPage";

/** The one preset the assertions target. */
const TARGET = "playwright";

interface Deferred {
  resolve: (value: unknown) => void;
  promise: Promise<unknown>;
}

function deferred(): Deferred {
  let resolve!: (v: unknown) => void;
  const promise = new Promise((res) => {
    resolve = res;
  });
  return { resolve, promise };
}

let installDeferred: Deferred;
/** What `GET /api/mcp-catalog` returns — a test flips this to simulate the
 *  post-install catalog reload. */
let catalogServers: unknown[] = [];

/** Route a fetch stub: catalog list, install status poll, and the install itself. */
function stubFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string) => {
      if (url.endsWith("/api/mcp-catalog")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve({ servers: catalogServers }),
        });
      }
      if (url.endsWith("/api/mcp-catalog/install") && !url.includes("/status")) {
        return installDeferred.promise.then((body) => ({
          ok: true,
          status: 200,
          json: () => Promise.resolve(body),
        }));
      }
      if (url.includes("/api/mcp-catalog/install/") && url.endsWith("/status")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({ name: TARGET, running: true, stage: "installing", elapsed_ms: 4000 }),
        });
      }
      return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({}) });
    }),
  );
}

/** The target preset's row. */
function row(): HTMLElement {
  return screen.getByTestId(`mcp-preset-${TARGET}`);
}

/** The Install button, or null when the row has no clickable action left. */
function installButtonOrNull(): HTMLElement | null {
  return within(row()).queryByRole("button", {
    name: new RegExp(`^${i18n.t("harnessMcp.install")}`, "i"),
  });
}

/** The Install button for the target row. */
function installButton(): HTMLElement {
  return within(row()).getByRole("button", {
    name: new RegExp(`^${i18n.t("harnessMcp.install")}`, "i"),
  });
}

async function clickInstall() {
  await act(async () => {
    fireEvent.click(installButton());
  });
}

describe("McpTab install progress (ADR-072)", () => {
  beforeEach(() => {
    installDeferred = deferred();
    catalogServers = [];
    useMcpStore.setState({
      catalog: [],
      loading: false,
      error: null,
      installing: [],
      installStages: {},
      installElapsed: {},
      installOutcomes: {},
    } as never);
    stubFetch();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows an in-row busy state and no dialog while the install is in flight", async () => {
    render(<McpTab />);
    await clickInstall();

    // The row owns the busy state: spinner + label, and the button is gone
    // — a second install cannot be fired from the UI.
    expect(within(row()).getByText(i18n.t("harnessMcp.installing"))).toBeTruthy();
    expect(installButtonOrNull()).toBeNull();

    // Stage + elapsed come from the Gateway status poll. The two stage
    // labels differ only by the ellipsis, so match the full string.
    const stage = await screen.findByTestId(`mcp-install-stage-${TARGET}`);
    expect(stage.textContent).toContain(i18n.t("harnessMcp.installStage_installing"));
    expect(stage.textContent).toContain("4");

    // No modal anywhere — the old dialog was dismissable and left the row
    // looking untouched while the request kept running.
    expect(document.querySelector(".bg-modal-overlay")).toBeNull();
  });

  it("renders a failed install inline and offers Retry", async () => {
    render(<McpTab />);
    await clickInstall();

    await act(async () => {
      installDeferred.resolve({
        name: TARGET,
        success: false,
        stdout: "",
        stderr: "Missing runtime 'uvx'. Install it first, then retry.",
        install_duration_ms: 12,
      });
      await installDeferred.promise;
    });

    // Failure is visible on the row, and the action becomes Retry.
    expect(within(row()).getByText(i18n.t("harnessMcp.installFailed"))).toBeTruthy();
    expect(within(row()).getByText(/Missing runtime 'uvx'/)).toBeTruthy();
    expect(
      within(row()).getByRole("button", {
        name: new RegExp(i18n.t("harnessMcp.installRetry"), "i"),
      }),
    ).toBeTruthy();

    // Busy state is gone.
    expect(screen.queryByTestId(`mcp-install-stage-${TARGET}`)).toBeNull();
  });

  it("clears the busy state when the install succeeds", async () => {
    render(<McpTab />);
    await clickInstall();

    // On success the store reloads the catalog; the Gateway has by then
    // written the entry, so the row renders the green "Installed" badge.
    // `McpCatalogEntryResponse` is `#[serde(flatten)]` — `name` is top level.
    catalogServers = [{ name: TARGET, transport: "stdio", command: "npx", args: [], env: {}, headers: {} }];

    await act(async () => {
      installDeferred.resolve({
        name: TARGET,
        success: true,
        stdout: "added 1 package",
        stderr: "",
        install_duration_ms: 30,
        tool_count: 12,
      });
      await installDeferred.promise;
    });

    expect(within(row()).getByText(i18n.t("harnessMcp.installed"))).toBeTruthy();
    expect(screen.queryByTestId(`mcp-install-stage-${TARGET}`)).toBeNull();
    expect(useMcpStore.getState().installing).toEqual([]);
  });
});
