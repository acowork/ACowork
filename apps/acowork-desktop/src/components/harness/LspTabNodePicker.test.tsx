//! Regression guard for the harness LSP node picker.
//!
//! Two separate regressions are pinned here:
//!
//! 1. The picker used to sit as a free-floating label + listbox above the
//!    LSP card, on the panel background. Every other control in the
//!    settings/harness surface lives inside a card, so it read as an
//!    orphaned form. It now rides in the card header's trailing slot next
//!    to Refresh.
//! 2. With no addressable node the component used to early-return before
//!    rendering the card at all, which made the picker unreachable and
//!    stranded the panel with no way to try another node. The card must
//!    always render, with the reason shown in its body. Since ADR-091 the
//!    candidate set is every ONLINE node — the registry is served by the
//!    Node itself and stays readable while its relay is down.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import i18n from "../../i18n";
import { useGatewayStore } from "../../stores/gatewayStore";
import { useAgentStore } from "../../stores/agentStore";
import { invalidateLspRelayEndpointCache } from "../../lib/gateway-api";
import { LspTab } from "./LspTab";

const NODE_A = "aaaaaaaa-1111-2222-3333-444444444444";
const NODE_B = "bbbbbbbb-1111-2222-3333-444444444444";

interface NodeFixture {
  node_id: string;
  node_name: string;
  online: boolean;
  http_endpoint: string;
}

/** Nodes returned by `GET /api/nodes`. */
let nodes: NodeFixture[] = [];
/** Hosting node reported by `GET /api/agents/{id}/lsp-endpoint`.
 *  ADR-089 D3: the client assembles the usable relay URL as
 *  `{gateway}/api/nodes/{node_id}/lsp` from this id; `null` = no relay. */
let agentNodeId: string | null = null;

/** `getLspRelayUrl` memoises per agent id in a module-level map, so each
 *  case needs its own id or an earlier case's endpoint leaks into it. */
let agentSeq = 0;
let agentId = "";

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
              node_id: agentNodeId,
              endpoint: agentNodeId
                ? `http://localhost:19876/api/nodes/${agentNodeId}/lsp`
                : null,
              ready: agentNodeId !== null,
            }),
        });
      }
      if (url.includes("/lsp-admin/servers")) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () =>
            Promise.resolve({
              version: 1,
              servers: {
                rust: {
                  source: "preset",
                  candidates: ["rust-analyzer"],
                  args: [],
                  install_hint: "rustup component add rust-analyzer",
                  description: "Rust",
                  install_state: "detected",
                },
              },
              conflicts: [],
              installing: [],
            }),
        });
      }
      if (url.includes("/api/lsp/status")) {
        // `fetchLspStatus` returns a bare array of entries, not a map.
        return Promise.resolve({
          ok: true,
          status: 200,
          json: () => Promise.resolve([{ language: "rust", installed: true }]),
        });
      }
      if (url.includes("/api/nodes")) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(nodes) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({}) });
    }),
  );
}

/** The card header row — the clickable fold control that carries the picker.
 *  The count badge varies with the loaded list, so match the title alone. */
function cardHeader(): HTMLElement {
  return screen.getByRole("button", { name: /^LSP Server Management/ });
}

function picker(): HTMLSelectElement {
  return screen.getByLabelText(i18n.t("harnessLsp.node")) as HTMLSelectElement;
}

describe("LspTab node picker placement (ADR-055 6.7)", () => {
  beforeEach(() => {
    nodes = [
      { node_id: NODE_A, node_name: "desktop-a", online: true, http_endpoint: "http://192.168.5.82:19900" },
      { node_id: NODE_B, node_name: "desktop-b", online: true, http_endpoint: "http://192.168.17.113:19900" },
    ];
    agentNodeId = NODE_A;
    agentId = `com.example.agent-${++agentSeq}`;
    invalidateLspRelayEndpointCache();
    useGatewayStore.setState({ status: "connected" } as never);
    useAgentStore.setState({ selectedAgentId: agentId } as never);
    stubFetch();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the picker inside the card header, not as a standalone control", async () => {
    render(<LspTab />);
    await waitFor(() => expect(picker()).toBeTruthy());

    const header = cardHeader();
    expect(within(header).getByLabelText(i18n.t("harnessLsp.node"))).toBe(picker());
  });

  it("still renders the card when no node is online, so the picker stays reachable", async () => {
    nodes = [];
    agentNodeId = null;
    render(<LspTab />);

    await waitFor(() =>
      expect(screen.getByText(i18n.t("harnessLsp.noNodes"))).toBeTruthy(),
    );
    // The fold must still exist: without it the user has no way to reach
    // the picker at all.
    expect(cardHeader()).toBeTruthy();
  });

  it("names the host when the agent's node is outside the caller's manage list", async () => {
    // ADR-087 D5: a non-manager gets an endpoint but no node entry, so the
    // picker would otherwise render with an empty value.
    nodes = [
      { node_id: NODE_B, node_name: "desktop-b", online: true, http_endpoint: "http://192.168.17.113:19900" },
    ];
    agentNodeId = NODE_A;
    render(<LspTab />);

    await waitFor(() => expect(picker()).toBeTruthy());
    // The unnamed endpoint still appears as an option (empty value) so the
    // select is never blank.
    const values = Array.from(picker().options).map((o) => o.value);
    expect(values).toContain("");
    expect(picker().value).toBe("");
  });
});
