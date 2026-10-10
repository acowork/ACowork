//! Regression guard: install progress must survive a view switch.
//!
//! The report: click Install, switch away from the LSP settings, switch
//! back — the row reads "not installed" with no spinner and no result,
//! even though the install script was still running. `LspTab` unmounts on
//! every view switch (AppLayout renders views conditionally), so the
//! progress had to move out of component state into `lspInstallStore`.
//!
//! Also pinned: the header badge must not fall back to "not installed"
//! while an install is in flight, whatever the PATH probe last reported.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent, waitFor } from "@testing-library/react";
import i18n from "../../i18n";
import { useGatewayStore } from "../../stores/gatewayStore";
import { useAgentStore } from "../../stores/agentStore";
import { useLspInstallStore } from "../../stores/lspInstallStore";
import { invalidateLspRelayEndpointCache } from "../../lib/gateway-api";
import { LspTab } from "./LspTab";

const NODE_A = "aaaaaaaa-1111-2222-3333-444444444444";
const TARGET = "markdown";

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

/** The Node registry (`GET .../lsp-admin/servers`) serves `markdown` with an
 *  install script and no resolved install state, and the relay's PATH probe
 *  reports it missing — the state the user sees before and during a fresh
 *  install. */
function stubFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string) => {
      if (url.includes("lsp-endpoint")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({
              node_id: NODE_A,
              endpoint: `http://localhost:19876/api/nodes/${NODE_A}/lsp`,
              ready: true,
            }),
        });
      }
      // Ordered before the plain registry match: the install URL is a
      // sub-path of it.
      if (url.includes(`/lsp-admin/servers/${TARGET}/install`)) {
        return installDeferred.promise.then((body) => ({
          ok: true,
          status: 200,
          json: () => Promise.resolve(body),
        }));
      }
      if (url.includes("/lsp-admin/servers")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({
              version: 1,
              servers: {
                [TARGET]: {
                  source: "preset",
                  candidates: ["marksman"],
                  args: [],
                  install_script: TARGET,
                  install_hint: "brew install marksman",
                  description: "Markdown",
                  install_state: "unknown",
                },
              },
              conflicts: [],
              installing: [],
            }),
        });
      }
      if (url.includes("/api/lsp/status")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve([{ language: TARGET, installed: false }]),
        });
      }
      if (url.includes("/api/nodes")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve([
              { node_id: NODE_A, node_name: "desktop-a", online: true, http_endpoint: "http://x:19900" },
            ]),
        });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({}) });
    }),
  );
}

async function clickInstall(): Promise<void> {
  const button = await screen.findByRole("button", {
    name: new RegExp(`^${i18n.t("harnessLsp.install")}$`, "i"),
  });
  await act(async () => {
    fireEvent.click(button);
  });
}

describe("LspTab install progress survives a view switch", () => {
  beforeEach(() => {
    installDeferred = deferred();
    useLspInstallStore.setState({ installing: [], outcomes: {} } as never);
    invalidateLspRelayEndpointCache();
    useGatewayStore.setState({ status: "connected" } as never);
    useAgentStore.setState({ selectedAgentId: `com.example.a-${Math.random()}` } as never);
    stubFetch();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("still shows the row as installing after unmount + remount", async () => {
    const first = render(<LspTab />);
    await clickInstall();

    // The user leaves the Harness view: AppLayout drops LspTab entirely.
    first.unmount();

    render(<LspTab />);

    // Back in the panel: the badge is the amber installing state, not the
    // red "not installed" the PATH probe reported mid-download.
    await waitFor(() =>
      expect(screen.getByTestId("lsp-installing-badge")).toBeTruthy(),
    );
    expect(screen.queryByText(i18n.t("harnessLsp.notInstalled"))).toBeNull();
    expect(useLspInstallStore.getState().installing).toEqual([TARGET]);
  });

  it("renders the outcome on the row after a remount", async () => {
    const first = render(<LspTab />);
    await clickInstall();
    first.unmount();

    render(<LspTab />);
    // Wait for the remounted panel to finish loading its list — but on
    // the busy state itself, which test 1 owns. Asserting only here keeps
    // a badge regression from masking an outcome regression.
    // Wait for the remounted panel to finish loading its list. Anchor on the
    // candidate command, which renders in every version of this component —
    // waiting on the install badge (test 1's subject) or the row testid
    // (added by the same fix) would let one regression mask the other.
    await waitFor(() => expect(screen.getByText("marksman")).toBeTruthy());

    await act(async () => {
      installDeferred.resolve({
        name: TARGET,
        success: true,
        installState: "installed",
        willRestart: false,
        exitCode: 0,
        stdout: "OK: marksman installed",
        stderr: "",
      });
      await installDeferred.promise;
    });

    // The result outlived the unmount too, so the user still sees the
    // outcome instead of an empty row. The raw installer output sits
    // behind a toggle — open it, then assert it survived as well.
    await waitFor(() =>
      expect(screen.getByText(i18n.t("harnessLsp.installSuccess"))).toBeTruthy(),
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Show output" }));
    });
    expect(screen.getByText("OK: marksman installed")).toBeTruthy();
    expect(useLspInstallStore.getState().installing).toEqual([]);
  });
});