# ADR-089: LSP Relay Reachability vs. Exposure — Desktop Accesses the Relay Through a Gateway Reverse Proxy

> **Chinese source of truth**: [ADR-089](../zh/ADR-089-lsp-relay-reachability-vs-exposure.md)

**Status**: Proposed (pending architecture review)
**Date**: 2026-10-09
**Decision makers**: pending review (user direction: restore reachability first, then close the exposure surface via reverse proxy)

**Related**:
- [ADR-055](../zh/ADR-055-remote-runtime-node-topology.md) (§6.3 endpoint/advertise model D3, §6.4 two-hop Runtime access D2, §6.7 Sidecar Scope model, §6.8 security model, §6.3.3 dynamic address self-healing)
- [ADR-019](../zh/ADR-019-lsp-relay-standalone-process.md) (LSP Relay as a standalone process)
- [ADR-030](../zh/ADR-030-sidecar-endpoint-dynamic-push.md) (sidecar endpoint dynamic push)
- [ADR-080](../zh/ADR-080-gateway-advertise-host-ip-change-watchdog.md) (advertise_host drift self-healing)
- [ADR-087](../zh/ADR-087-node-agent-owner-permissions.md) (Node / Agent ownership and permissions)
- [ADR-076](../zh/ADR-076-multi-user-account-system.md) / [ADR-084](../zh/ADR-084-user-standalone-process.md) (Desktop-side credential shape)

---

## 1. Decision Summary

### 1.1 One sentence

**The LSP relay's only consumer is the Desktop browser, and the relay process performs zero authentication — so it must not be exposed to the LAN.**
End state: Desktop reaches the relay exclusively through a **Gateway reverse proxy** (the same two-hop shape as §6.4 Runtime access), the relay binds back to `127.0.0.1`, and
the `acowork/nodes/{id}/lsps` retained topic becomes a health signal only — **no consumer treats its advertised URL as directly connectable**.

### 1.2 Key decisions

| # | Decision | Content |
|---|----------|---------|
| D1 | Relay binds back to loopback | relay `--host` is always `127.0.0.1` (retracting the interim "bind follows advertise" default), keeping only an explicit escape hatch `expose_lsp_relay` |
| D2 | Gateway adds a relay reverse-proxy route | `/{...}/api/nodes/{node_id}/lsp/*` → `http://{node.proxy_endpoint}/...`, injecting `X-ACowork-Node-Token`, reusing the existing `http/proxy.rs` forwarding machinery (including WebSocket upgrade passthrough) |
| D3 | Endpoint assembly returns the proxy URL | `GET /api/agents/{id}/lsp-endpoint` returns `{gateway_advertise}/api/nodes/{node_id}/lsp` instead of the node's LAN address; the topic payload semantics stay unchanged (no Node-side change, old nodes stay compatible) |
| D4 | Online gating | `get_agent_lsp_endpoint` MUST check `n.online`; an offline node returns `ready:false` plus a reason code rather than a dead address |
| D5 | bind and advertise share one source | The Node reverse proxy binds from the §6.3.3 `live_advertise_host` snapshot (rebind on IP drift), making it identically the advertised value; the one-shot LAN probe on the `start` default path is deleted |
| D6 | Runtime codebase tool wiring | **Out of scope for this ADR** (see §2.4), handled separately |

---

## 2. Background and Problem

### 2.1 Field facts (2026-10 incident)

Topology: Gateway + Desktop on `192.168.5.82`; Node (`node_id=3e314b69`) + Desktop on `192.168.17.113`.
Symptom: clicking LSP in the harness panel shows no server list; the browser reports `Failed to fetch`.

Measured asymmetry: `192.168.17.113:19900` (Node reverse proxy, bound `0.0.0.0`) returns 200; `192.168.17.113:19878` (relay) times out / CLOSED-FILTERED.

### 2.2 Root-cause chain

1. **bind ≠ advertise (violates §6.3 D3)**: `acowork-node/src/sidecar/lsp_relay.rs` hardcoded the relay's `--host` to `127.0.0.1`, while the `lsps` topic advertised `http://{advertise_host}:19878`. The browser connects to the advertised value → hits its own machine → fails.
2. **Offline nodes still return a dead address**: `get_agent_lsp_endpoint` (`acowork-gateway/src/http/agents.rs:1088`) reads only `n.lsp_endpoint` and **never checks `n.online`**, with `ready = endpoint.is_some()`. The retained value survives node disconnection, so the panel receives an address that can never connect and can only surface it as a network error.
3. **Structural source of bind/advertise divergence**: `acowork-node/src/cli.rs` resolves the `start` default via a one-shot `detect_non_loopback_ipv4()` probe when `--addr` is absent, while advertise_host is continuously refreshed by the §6.3.3 if-watch snapshot. Two different sources guarantee divergence after a network change. (By contrast, the Gateway-managed spawn passes `--addr 127.0.0.1:{port}` explicitly at `gateway/node_manager.rs:563`, taking the other branch.)

### 2.3 The interim fix and its cost

The first-step fix (relay bind follows `cfg.advertise_host`) made the panel work immediately, at the cost of **exposing a completely unauthenticated process to the LAN**: the relay serves `POST /api/lsp/install/{language}` (executes install scripts), `GET /api/lsp/servers-with-status`, and the `/lsp/{language}` WebSocket (spawns LSP servers, reads and writes workspace files). Any host on the LAN can reach and use it.

### 2.4 Incidental finding: half of §6.7 was never implemented

§6.7 states the relay endpoint is distributed to "Runtime (codebase tool) and Desktop (Monaco)", but `acowork-runtime/src/.../agent_init.rs:892` hardcodes `let lsp_relay_endpoint: Option<String> = None;` (ADR-040 layering leftover). **CodebaseTool was never registered; the relay's sole consumer to date is Desktop.** This does not change this ADR's conclusion (a browser cannot inject custom headers, so option B fails regardless), but it means "Runtime calling the relay across the network" is an unvalidated assumed path requiring separate treatment.

---

## 3. Alternatives

| Option | Description | Verdict |
|--------|-------------|---------|
| A | Relay binds to the advertised address, exposed directly on the LAN (= first-step status quo) | Reachable, but an unauthenticated process sits on a shared network. Rejected as the end state; retained only as an explicit escape hatch |
| B | The relay validates a node token itself | **Infeasible**: the consumer is a browser; `fetch` / `WebSocket` cannot inject custom request headers; cookies are bound by same-origin and unusable cross-origin; the token could only land in a URL or front-end storage — i.e. shipping a long-lived credential to any Desktop |
| C | Desktop reaches the relay through a Gateway reverse proxy | **Selected** |
| D | Move the relay back to the Gateway machine (revert to the ADR-019 single-machine shape) | Violates §6.7's technical basis: `root_uri = file://{workspace_root}` in `acowork-lsp-relay/src/codebase.rs` requires the LSP server to share the machine with the workspace. Rejected |

---

## 4. Rationale

1. **Isomorphic with the existing path, no new protocol surface.** The Gateway already injects `X-ACowork-Node-Token` toward the Node reverse proxy (`http/proxy.rs:1170` / `:2595`) and the Node already validates it (`proxy/mod.rs:201`). The relay route is one more upstream route, not a third authentication system.
2. **Closing the exposure surface resolves the §6.7 / §6.8 contradiction outright.** §6.7 implicitly assumes consumers connect to the advertised value directly; §6.8's security model covers only MQTT CONNECT authentication, inbound validation on the Node reverse proxy, and the Gateway peer-IP allowlist — the relay appears in none of them. Keeping the relay non-routable makes both clauses self-consistent.
3. **It also dissolves the multi-node ambiguity.** The user's question — "with several remote nodes each running LSP, which one does the panel refresh?" — disappears: the URL is always Gateway-side, and the node is resolved through agent → node ownership (§6.7's `GET /api/agents/{id}/lsp-endpoint`). Cross-node comparison would need a panel-level switcher, which is a product decision, not a protocol one.

---

## 5. Design

### 5.1 Gateway reverse-proxy route

```
GET/POST/DELETE  /api/nodes/{node_id}/lsp/*
  → validate Desktop-side credentials (existing Gateway HTTP auth middleware, unchanged)
  → check caller visibility of node_id per ADR-087
  → node_registry[node_id] → proxy_endpoint; !online → 503 + reason code
  → inject X-ACowork-Node-Token, forward to http://{proxy_endpoint}/{...}
```

WebSocket: `/lsp/{language}` requires `Upgrade` / `Connection` passthrough. **Today it is the opposite**: `http/proxy.rs:2457` / `:2464` already classify `connection` and `upgrade` as hop-by-hop (`is_hop_by_hop_header`, tests at `:3177` / `:3184`), so the current proxy silently strips the upgrade headers and downgrades the request to a plain GET. P1 MUST add an explicit exemption path for WebSocket routes and forward `Sec-WebSocket-*`. This is the only technical risk in the plan and the first test P1 writes.

### 5.2 Endpoint assembly (D3)

The `lsps` retained topic payload format is **unchanged** (still the node-side `http://{advertise_host}:19878`); the Gateway discards its routing meaning inside `get_agent_lsp_endpoint` and assembles the proxy URL instead. Rationale: the topic is a node→Gateway health report, so changing its meaning would require touching Node plus every deployed old node; the proxy URL is fully derivable from `node_id` + Gateway `advertise_host` and never needed advertising.

The topic value's one remaining use: the Node's own proof that the relay is ready (one input to `ready`).

### 5.3 Online gating and reason codes (D4)

```rust
// today: let ready = endpoint.is_some();
// end state: an offline node must not yield a connectable address
if !n.online { return AgentLspEndpointResponse { endpoint: None, ready: false, reason: Some("node_offline") } }
```

Minimal reason-code set: `node_offline` / `relay_not_ready` / `no_lsp_sidecar`. The front end renders readable copy from these instead of `Failed to fetch`.

### 5.4 bind and advertise share one source (D5)

The Node reverse proxy MUST be reachable by the Gateway (the precondition for cross-machine deployment), so D5 is **not** "default `--addr` to loopback" — that would break remote nodes outright. The correct fix: bind from the §6.3.3 `live_advertise_host` snapshot, rebind on IP drift, always identical to what is advertised; delete the one-shot probe branch.

### 5.5 Escape hatch (D1)

`[node].expose_lsp_relay = false` (default). When true the relay binds to the advertised address — for fully isolated experimental networks only, and documentation MUST mark it as an unsupported configuration.

---

## 6. Consequences

### 6.1 Positive

- The relay returns to loopback; nothing reachable on the LAN. Script execution, LSP spawn, and workspace read/write all pass Gateway authentication and ownership checks.
- bind/advertise divergence is structurally removed (D5); a network change no longer produces "one port reachable, one not".
- The panel no longer depends on the node's LAN address, giving clear semantics under multiple nodes.
- Offline nodes produce readable errors instead of network timeouts.

### 6.2 Negative and costs

- The two-hop proxy adds latency per editor LSP request (local loopback hop, acceptable magnitude).
- The Gateway must support WebSocket passthrough (§5.1 risk).
- On remote nodes, Gateway → Node LSP traffic shares the reverse-proxy port (19900) bandwidth and connection ceiling with Runtime traffic.
- The already-merged first-step bind fix is retracted in P3 — an acceptable reversal (self-consistent within the same PR sequence).

### 6.3 Risks

| Risk | Mitigation |
|------|-----------|
| WS upgrade swallowed by the hop-by-hop filter, presenting as "all HTTP endpoints work, editor stays dark" | P1 writes the passthrough test first; verify with Monaco against a real server |
| rumqttd 0.20 has no topic-level ACL (§6.8 records this deviation), so any enrolled node can publish a forged `lsps` | Under D3 the Gateway trusts registry `node_id` ownership, not the topic value — immune by construction |
| Third parties / scripts that currently reach the relay directly break | The relay never had a stable external contract (its endpoint drifts with IP); not a broken interface. Note in CHANGELOG |

---

## 7. Implementation Boundary

| Phase | Content | Acceptance |
|-------|---------|-----------|
| P1 | Gateway relay reverse-proxy route + WS passthrough + node token injection | route unit tests; WS upgrade passthrough test; offline node → 503 |
| P2 | `get_agent_lsp_endpoint` returns the proxy URL + online gating + reason codes | multi-node case returns the right node's proxy URL; offline returns `ready:false` |
| P3 | Desktop switches to the proxy URL; relay bind back to `127.0.0.1`; `expose_lsp_relay` hatch | cross-machine field test: harness panel and Monaco editor both work |
| P4 | D5: Node reverse-proxy bind follows `live_advertise_host` | reachability restored after IP change without a restart (if-watch triggers rebind) |

**Out of scope**: Runtime codebase tool wiring (§2.4, separate); Phase 5b untrusted-network tier (TLS / payload encryption); topic-level ACL (depends on broker replacement evaluation).

---

## 8. Rollback

P1/P2 are purely additive routing and assembly logic. Rollback = return the topic-advertised value again and let the relay bind follow advertise (i.e. the first-step status quo: functional, exposure surface reopened). The rollback switch is §5.5's `expose_lsp_relay`; no code revert needed.

---

## 9. Open Questions

1. Granularity of ownership checks on the LSP proxy route: per agent or per node? (Before ADR-087 is finalized, P1 reuses existing node visibility gating.)
2. Does the panel need a cross-node comparison view? Current semantics are "the node the agent lives on"; a full multi-node overview is a product decision.
3. Once the Runtime codebase tool is wired, the path becomes Runtime → Gateway → Node → relay (three hops). Should the Runtime reach the Node reverse proxy directly, and who issues the node token it would carry — separate ADR.
