/**
 * `getLspRelayUrl` must resolve in relay mode.
 *
 * Why this file exists:
 *   57b3d73b ("degrade node-local features in relay mode", design doc 24
 *   F7) added `if (getGatewayMode() === "relay") return null;` on the
 *   assumption that the relay's node-local HTTP endpoint cannot be
 *   reached through the cloud tunnel. That guard is wrong: the editor's
 *   LSP WebSocket tunnels fine in relay mode (diagnostics pass,
 *   rust-analyzer reaches `ready`), and this function serves only the
 *   DIRECT HTTP callers — the harness LSP panel, project-root discovery
 *   and the install-script runner. With the guard in place the harness
 *   panel reported "relay not available" on every relay connection, so
 *   the feature looked broken in exactly the mode users remote-work in.
 *
 * What this pins: the guard is gone, and the function no longer reads
 * the gateway mode at all. A mode-based branch here is the specific
 * regression — it would silently re-disable the panel again.
 *
 * Why source scanning: `getGatewayMode` reads localStorage at module
 * level and the endpoint cache is process-wide, so asserting on rendered
 * output would need Tauri plus a live gateway. The absence of the branch
 * IS the contract.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));
const src = readFileSync(resolve(__dirname, "gateway-api.ts"), "utf8");

/** The function body only — the module header imports other symbols. */
const body = src.match(
  /export async function getLspRelayUrl\([\s\S]*?\n\}/,
)![0];

describe("getLspRelayUrl resolves in relay mode", () => {
  it("has no gateway-mode short-circuit", () => {
    // The regression: a mode check that returns null before the endpoint
    // is ever fetched. It must not come back.
    expect(body).not.toContain("getGatewayMode");
  });

  it("resolves through the shared endpoint cache", () => {
    // The cache is what dedupes concurrent callers (panel + project-root
    // + indicator all fire on mount) and what clears itself on error.
    expect(body).toContain("getCachedLspRelayEndpoint(agentId, gatewayUrl)");
  });

  it("still short-circuits when no agent is given", () => {
    // The relay is a per-agent sidecar (ADR-055 §6.7) — with no agent
    // there is nothing to resolve, and the panel must degrade rather
    // than fetch a meaningless endpoint.
    expect(body).toMatch(/if \(!agentId\) return null;/);
  });

  it("no longer imports getGatewayMode for this path", () => {
    // If nothing else needs it, the import itself is dead weight that
    // hints the branch is coming back.
    expect(src).not.toMatch(/import \{[^}]*\bgetGatewayMode\b[^}]*\}/);
  });
});
