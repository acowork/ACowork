# ADR-072: Generic MCP Install Detection Module — Declarative PackageSpec

> **Chinese source of truth**: [ADR-072](../zh/ADR-072-mcp-install-package-spec.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Accepted

## Date

2026-09-22

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-069](./ADR-069-mcp-tool-level-optin.md) — per-tool MCP opt-in; this ADR adds the
  "server install" half of the MCP story
- [ADR-055](./ADR-055-remote-runtime-node-topology.md) — Node Agent; MCP install may
  eventually distribute across machines, so PackageSpec stays machine-independent
- [04-mcp.md](./ADR-072-mcp-install-package-spec.md) — the stdio handshake convention
- LSP install scripts (`acowork-lsp-relay/src/install.rs` + `assets/lsp_install/`) — the
  **counterpart** this ADR deliberately diverges from

---

## Context

The desktop MCP panel keeps a hardcoded preset list in
`apps/acowork-desktop/src/lib/mcp-presets.ts`, where each entry has only `command`,
`args`, and `installHint` (prose). Clicking "add" makes the Gateway `POST /api/mcp-catalog`
and probe — and **if the runtime a preset assumes is missing, the user has no way to
recover.**

**Case in point — the docling preset, three errors (measured 2026-09-22).** Adding
docling failed with `failed to create transport ... program not found`. Checking each item
against the downloaded PyPI wheel:

| # | Problem | Fact |
|---|---------|------|
| 1 | Wrong command name | the preset says `uvx docling-mcp`, but the console_script of docling-mcp v3.2.0 is `docling-mcp-server` (confirmed in entry_points.txt) — package name ≠ command name |
| 2 | Missing stdio flag | `mcp_server.py` defaults to `transport=STREAMABLE_HTTP` (localhost:8000), so the ACowork stdio handshake gets no response; it needs an explicit `--transport stdio` |
| 3 | Missing runtime | the machine has no `uvx` (`where uvx` finds nothing) and the preset offers no install path |

The probe HTTP fallback only tries `[3333, 3000, 8080]`, which excludes docling default 8000,
so even a running process would not be detected. **The built-in preset list has completely
failed at promising that installation succeeds.**

**The LSP counterpart.** LSP already has the full mechanism (`install_script` field →
`assets/lsp_install/{lang}.{ps1,sh}` → `GET/POST /api/lsp/install/{lang}` → one-click install
with live output). But LSP scripts are **imperative**: one script per language hardcoding package
manager, package identity, PATH handling, and health check. That fits LSP (a closed, stable
set of languages) and does **not** fit MCP, an open ecosystem where the community ships new servers daily:
copying LSP would mean two scripts per new preset, and it is useless for custom MCP servers.

## Goals

1. **Generic install detection** — given a package identity, the framework derives the install
   command, derives the spawn command, runs a uniform health check, and handles PATH,
   **bound to no specific MCP server**.
2. **Works out of the box** — a built-in preset must either install successfully or give a
   clear guided path; the user is never left to solve dependencies outside ACowork.
3. **Declarative first, scripts as fallback** — 95% of MCP servers are expressed by a
   declarative `McpPackageSpec`; only complex cases (playwright also needs a browser) fall
   back to `install_script`.
4. **Custom MCP unaffected** — manually entered servers (local binary / HTTP URL) behave as
   before.
5. **Safe** — the install endpoint accepts only a structured PackageSpec and rejects
   arbitrary shell strings (injection guard).

## Alternatives

**A — imperative scripts, copying LSP.** Hand-write `.ps1` / `.sh` per preset, each with
Install → Verify → Health-Check phases. Rejected: it is tightly coupled to each server
(a new preset costs two scripts, ~200 lines each), each script mixes package manager,
package identity, PATH, and health check so changing a package name or a release method
means editing the script, and custom MCP servers cannot use it at all.

**B — declarative PackageSpec (chosen).** Split installation into three orthogonal
dimensions: package manager (kind) × package identity (spec) × launch method (entry_point
/ spawn_args). A preset declares only facts; the framework owns the whole flow
(detect dependency → install → PATH handling → health check → idempotency). A new preset
costs a few lines of JSON, custom MCP servers reuse the same mechanism, and health check,
timeouts, and PATH are implemented once. The cost is a new abstraction layer, with
`install_script` as the fallback for cases it cannot express.

**C — hybrid (a refinement of B, also chosen).** B is the main path, while
`PackageKind::Script` keeps an `install_script` field as the escape hatch for special
cases such as installing a browser for playwright. The health check is always performed by
the framework and never reimplemented inside a script.

## Decision

**Decision 1 — declarative `McpPackageSpec` (Options B + C).**

```rust
/// MCP install declaration: package manager × package identity × launch method,
/// three orthogonal axes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPackageSpec {
    /// Distribution channel, deciding the install command derivation and
    /// dependency detection strategy.
    pub kind: PackageKind,
    /// Package identity: npm name / pypi name / cargo crate / docker image /
    /// binary URL / git URL.
    pub spec: String,
    /// pypi only: uvx / pipx / pip, deciding the runner and PATH handling.
    pub runner: Option<PypiRunner>,
    /// Overrides the spawn command name (defaults to `spec`).
    /// docling needs "docling-mcp-server".
    pub entry_point: Option<String>,
    /// Arguments appended after the spawn command. docling needs
    /// ["--transport", "stdio"].
    pub spawn_args: Vec<String>,
    /// Fully overrides the spawn (command + args). Used when a server such as
    /// filesystem needs a working-directory argument.
    pub exec_override: Option<ExecOverride>,
}

pub enum PackageKind { Npm, Pypi, Cargo, Go, Docker, Binary, Script }
pub enum PypiRunner { Uvx, Pipx, Pip }
```

**Key criterion**: the spawn command MUST NOT be derived from the package name by
default. Measured against docling, three names differ — package `docling-mcp`, entry
point `docling-mcp-server`, and a required `--transport stdio` — so `entry_point` /
`spawn_args` / `exec_override` are necessary fields.

**Decision 2 — command derivation matrix, implemented once in the framework.**

| kind | dependency check | install command | spawn command |
|------|------------------|-----------------|---------------|
| Npm | `npx` present | `npx -y <spec>` (warms the cache) | `npx -y <spec> <spawn_args>` |
| Pypi+Uvx | `uvx` present | `uvx --from <spec> <entry_point>` warm-up | `uvx --from <spec> <entry_point> <spawn_args>` |
| Pypi+Pipx | `pipx` present | `pipx install <spec>` | `<entry_point> <spawn_args>` (already on PATH) |
| Pypi+Pip | `pip` present | `pip install <spec>` | `python -m <entry_point> <spawn_args>` |
| Cargo | `cargo` present | `cargo install <spec>` | `<entry_point> <spawn_args>` |
| Go | `go` present | `go install <spec>@latest` | `<entry_point> <spawn_args>` |
| Docker | `docker` present | `docker pull <spec>` | `docker run -i --rm <spec> <spawn_args>` |
| Binary | — | download → extract → register on PATH | `<entry_point> <spawn_args>` |
| Script | — | run `install_script` (.ps1/.sh) | per `McpServerConfigDef.command/args` |

When the dependency check fails (e.g. `uvx` missing), return **structured guidance**
("install uv first: `pip install uv`") and do **not** auto-install the underlying
runtime — a confirmed user-agency principle. The user clicks Install again once it is
present.

**Decision 3 — layer the catalog; `McpServerConfigDef` is untouched.** That struct
(name/transport/url/command/args/env/headers) stays exactly as is, remaining the spawn wire
shape and keeping custom MCP compatible. The optional addition is:

```rust
/// Optional install context for a catalog entry (written alongside the preset).
pub struct McpInstallSpec {
    pub package: McpPackageSpec,
    /// Install state, driving the frontend Install/Repair button state.
    pub state: InstallState,
}
```

A custom MCP server omits `install` and follows the old path unchanged; adding a
preset persists the spawn config together with the install context.

**Decision 4 — the framework always performs the health check.** Every kind reuses the
`McpClient::initialize` JSON-RPC handshake already implemented in `acowork-mcp`, never
reimplemented inside a script. docling loads its model **lazily**
(`LocalDocumentConverter._converter = None`, downloaded on first conversion), so the
handshake returns quickly and the 60s idle timeout of `run_command_with_idle_timeout`
(`core/acowork-core/src/process.rs`) is safe.

**Decision 5 — the `do_probe` HTTP fallback port becomes configurable.** It currently
hardcodes `[3333, 3000, 8080]`. Read the default port from `McpPackageSpec.spawn_args` or an extra
field (docling is 8000), and prefer the HTTP fallback when stdio fails and `spawn_args`
contains `--transport stdio`.

**Decision 6 — security boundary.** `POST /api/mcp-catalog/install/{name}` accepts only the
`McpInstallSpec` of a **catalog-registered entry**, or a **structured** `McpPackageSpec`
sent back by the frontend with `kind` allowlist validation. Arbitrary shell string
concatenation is rejected.

**Decision 7 — idempotency.** When the catalog already has an entry of the same name with
`state=installed`, the frontend hides the Install button or turns it into "Repair" (re-detect
and reinstall). Writing to the catalog is allowed only after a successful install — blocking, to
honour the works-out-of-the-box commitment.

**Decision 8 — preset migration.** `McpPresetDef` in `mcp-presets.ts` gains
`install?: McpInstallSpec`. docling becomes the first case and fixes all three errors at once:

```ts
{
  id: "docling",
  transport: "stdio",
  package: {
    kind: "pypi",
    spec: "docling-mcp",
    runner: "uvx",
    entry_point: "docling-mcp-server",
    spawn_args: ["--transport", "stdio"],
  },
  // the framework derives the spawn config from `package`; no hand-written command/args
}
```

Pure npm presets (playwright / context7) migrate to `{ kind: Npm, spec: "@playwright/mcp@latest" }`
with behaviour unchanged.

## Consequences

**Upside**

- Built-in presets genuinely work out of the box: declarative install plus a uniform health
  check, with clear guidance on failure.
- A new MCP preset costs a few lines of JSON instead of two platform scripts, and custom MCP
  servers can reuse the install mechanism.
- PATH handling, timeouts, idempotency, and health check live in one place, so tests
  concentrate there.
- The three docling errors (command name / stdio / missing uv) are fixed together.

**Downside / cost**

- A new abstraction layer (`McpPackageSpec` plus the derivation matrix), roughly 4.5 days for
  the first implementation.
- Cases the matrix cannot express need `install_script` as a fallback (the mechanism is
  reserved now; the special cases are not implemented).
- Existing catalog entries have no `install` field, so reads default to `None` with no migration
  code, consistent with the project `deny_unknown_fields` convention.

**Rollback**

- `McpInstallSpec` is an optional extension of `McpServerConfigDef`; deleting the `install` field
  reverts it and spawn behaviour is unchanged.
- Preset migration can be reverted entry by entry to the old `command/args` form.

## Test strategy (P5)

- Unit: the `PackageSpec` → install/spawn command derivation matrix as a pure function
  covering every kind; each dependency-detection branch.
- Integration: a mock runner simulating `uvx` present and absent; a mock MCP server
  verifying the health check handshake.
- End to end: real docling (`uvx --from docling-mcp docling-mcp-server --transport stdio`)
  install, probe, and catalog insert.

## Impact

| File | Change |
|------|--------|
| `acowork-core/src/protocol.rs` | new `McpPackageSpec` / `PackageKind` / `PypiRunner` / `McpInstallSpec` types with serde |
| `acowork-mcp/src/install.rs` | the generic installer: command derivation, dependency detection, health check, idempotency, safety allowlist |
| `assets/mcp_install/` | optional script fallback directory, isomorphic to `assets/lsp_install/` |
| `acowork-gateway/src/http/mcp_catalog_api.rs` | `GET/POST /api/mcp-catalog/install/{name}`; `do_probe` HTTP fallback port configurable |
| `apps/acowork-desktop/src/lib/types.ts` | `McpPresetDef` gains `install?` |
| `apps/acowork-desktop/src/lib/mcp-presets.ts` | presets move from `command/args/installHint` to the declarative `package` |
| `apps/acowork-desktop/src/stores/mcpStore.ts` | new `installMcp` action |
| `apps/acowork-desktop/src/components/harness/HarnessPage.tsx` | Install button plus dialog in McpTab, UX modelled on LspTab |
