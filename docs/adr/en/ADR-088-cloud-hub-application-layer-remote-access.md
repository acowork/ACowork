# ADR-088: Adding the Cloud Hub Application-Layer Remote-Access Mode, Coexisting Permanently with the Thin Relay (Design Doc 24)

> **Chinese source of truth**: [ADR-088](../zh/ADR-088-cloud-hub-application-layer-remote-access.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

**Status**: Decided (not yet implemented; Phase 0 not started)
**Date**: 2026-10-08
**Decision Makers**: Architecture review (user's decision: keep the relay mode, add the Cloud Hub mode; the two coexist and users choose — relay is **not** removed)

**Related**:
- [Design doc 24](../../design/zh/24-cloud-relay-remote-access.md) (cloud thin relay — **this ADR does not supersede it; it repositions it as the self-hosted BYO tier and freezes it**)
- [Design doc 26](../../design/zh/26-cloud-sync-hub.md) (the full design for this ADR)
- [ADR-055](./ADR-055-remote-runtime-node-topology.md) (Gateway converges onto pure networking duties; the Gateway boundary red line)
- [ADR-075](./ADR-075-node-identity-uuid-and-node-name.md) (the `identity.json` device-key precedent — both modes share its credential form)
- [ADR-076](./ADR-076-multi-user-account-system.md) / [ADR-084](./ADR-084-user-standalone-process.md) (multi-user accounts and the user-domain service)
- [ADR-048](./ADR-048-debug-protocol-mqtt-http.md) (DevMode debug protocol — explicitly never exposed remotely)
- Closed-source repo: overall architecture (C1–C4 hard rules, D1 cross-customer interop, D2 multi-tenancy, D5 billing) — not distributed with this repo
- Closed-source repo: the multi-tenant SaaS implementation layer of the Hub — not distributed with this repo

---

## 1. Decision Summary

### 1.1 In one sentence

**Add a second remote-access mode besides the "byte tunnel": Cloud Hub — a stateful backend service that understands message semantics.**
The Gateway side makes only outbound short HTTPS requests (long-poll for commands + POST events carrying a `seq`);
Desktop / Mobile use plain REST + SSE. The two sides meet at the **command/event semantic layer**, not at the TCP byte layer.
**It coexists permanently with the thin relay of doc 24 and users choose; it is not a replacement**: Relay = the self-hosted
BYO tier (bring your own VPS; zero cloud cost for us, zero compliance exposure, relay holds no secrets),
Hub = the default hosted tier (offline messages, cross-device sync, push, audit).
Coexistence is conditional on **four hard constraints P1–P4** (a single capability manifest, a single device identity,
a single client abstraction, transport-agnostic conformance tests) and on **freezing the relay protocol at `proto=1`**
(security fixes only thereafter).

### 1.2 Key decisions

| # | Decision | Outcome |
|---|---|---|
| 1 | Relay abstraction level | From "protocol-agnostic byte pipe" to "application-layer messaging (commands + events)" — **add one mode, do not replace** |
| 2 | Source of truth | **The Gateway remains the sole authority.** The Hub holds only two kinds of bounded, explicitly non-authoritative data: a TTL-bounded delivery buffer, and a projection cache stamped with `source_version` |
| 3 | Remote MQTT | **Removed entirely in Hub mode** (no `:19874` remote listener, no `/mqtt` WS→TCP bridge, no per-origin ACL). Retained in Relay mode (frozen) |
| 4 | Capability surface | **A single `RemoteSurfaceManifest`** (measured: 32 path templates -> ~30 ops + 6 streams; the surface spans 4 services, reached via the Gateway loopback proxy reusing existing routes) that **generates both the Hub registry and the Relay allowlist**, pinned by a CI drift check |
| 5 | Identity layering | The Hub layer authenticates "who you are / whether this device is paired"; the Gateway layer adjudicates "may you do this". The Hub is **not trusted for authorization** |
| 6 | Command forgery | Clients **sign commands end-to-end with Ed25519**; public keys are registered at the Gateway. A fully compromised Hub **cannot issue any valid command** to the Gateway |
| 7 | Content confidentiality | **E2E envelope encryption on by default** in hosted form (the Hub stores ciphertext only); self-hosted may disable it explicitly |
| 8 | Downlink transport | **SSE with long-polling fallback, no WebSocket**; uplink is short-lived POST; the Gateway fetches commands via long polling (preserving the outbound-only invariant) |
| 9 | Streaming output | Events are batch-merged (time and count thresholds); **the SSE stream is never proxied as a byte stream** |
| 10 | Relay evolution | **Frozen**: the previously planned QUIC transport and multi-instance Redis sharding are cancelled; wildcard device subdomains and SNI routing stay as they are |
| 11 | Open/closed boundary | Protocol and single-tenant reference implementation in the open repo; multi-tenancy / billing / vendor push / geo-admission / domestic deployment in the closed repo |

---

## 2. Background

Two problems with the **same root cause** surfaced while doc 24 was being implemented:

1. **Network**: the tunnel is a long-lived, multiplexed, non-standard-fingerprint connection
   (WSS + yamux + wildcard device-subdomain SNI routing). In restricted network environments this is exactly the
   kind of connection most easily identified and cut; when it is cut, **every multiplexed stream on the yamux session
   dies at once**, and because the tunnel holds no state there is **no recovery point at all**.
2. **Commercial**: a byte pipe **has no product surface**. Offline messages, cross-device unread sync, push,
   auditing, usage-based billing — all of these require the intermediary to understand semantics, whereas
   "not understanding semantics by design" is precisely doc 24's core virtue. The two cannot be reconciled.

At the same time, doc 24's technical core (single source of truth, broker stays at the Gateway, relay holds no secrets)
is **correct** and should not be overturned.

---

## 3. Alternatives

| Option | Outcome | Reason |
|---|---|---|
| **A. Cloud Hub application-layer hub, coexisting with Relay** | ✅ **Adopted** | Recoverability comes from message-level cursors; the commercial surface grows on this layer; it also removes the four most complex pieces of remote-MQTT code; Relay keeps its BYO advantages |
| A′. Hub replaces and deletes Relay | ❌ Rejected (user's decision) | Relay's "zero cloud cost for us + zero compliance exposure + relay holds no secrets" are advantages Hub **can never have**; deleting it hands the privacy-first and bring-your-own-VPS audience to competitors |
| B. Harden the tunnel only (domestic VPS / QUIC / CDN in front) | ❌ Rejected as an end state | A long-lived multiplexed connection with wildcard subdomains remains the most recognizable fingerprint, and **the investment cannot be recouped** (no commercial product surface) |
| C. Cloud becomes the source of truth (Gateway syncs upward) | ❌ **Explicitly rejected** | Dual sources of truth — **structurally identical** to the reasons doc 24 §3.2 rejected "public MQTT broker + bridge" |
| D. Push + polling only | ❌ Rejected as the main channel | Latency is unacceptable for interactive flows such as tool approval / ask-question; **retained as an auxiliary channel inside A** (push only wakes the client, it carries no content) |

---

## 4. Rationale

### 4.1 Why coexistence rather than replacement

The two modes serve **non-overlapping** audiences and constraints:

| | Relay (doc 24) | Cloud Hub (doc 26) |
|---|---|---|
| Who operates it | The user's own VPS | Officially hosted / enterprise self-hosted |
| Our cloud cost | **Zero** | We pay for storage and bandwidth |
| Compliance exposure | We hold no traffic | We are the service provider |
| Usability under restricted networks | ⚠️ Poor | ✅ Good |
| Unique capabilities | Any-protocol passthrough (new protocols need no changes) | Offline messages, cross-device sync, push, audit |

**Key judgement**: Relay is not legacy debt — it is the **BYO tier**. Removing it means pushing the audience
that needs the least infrastructure from us, and carries the least compliance risk, to a competitor.

### 4.2 Why coexistence must be constrained (P1–P4)

The biggest risk of coexistence is not cost, but **two implementations, two security models and two bug surfaces for
the same remote capability**. Each constraint closes one specific path to degradation:

| Constraint | Degradation it closes |
|---|---|
| **P1 single Manifest** | Capability drift (switching mode loses features) + the relay's permanent control-plane exposure risk |
| **P2 single device identity** | Two credential sets → revocation misses one side → revoked devices keep access |
| **P3 single `RemoteClient` abstraction** | Mode branches in business code → every feature written twice → god-module |
| **P4 transport-agnostic conformance tests** | Behavioural drift going unnoticed — **the only mechanism that can prove coexistence is not rotting** |

**P1 is the most important piece of new work in this decision**: it requires the Relay path to gain an allowlist gate
(today it forwards arbitrary bytes by design). This is not a tunnel rewrite — it is a path/topic allowlist on the
Gateway `remote_listener`, generated from the same manifest. In doing so, doc 24 §3.2 item 3
("one misconfigured topic filter rule exposes the control plane publicly") moves from
**"depends on configuration being correct" to "impossible by construction"**.
**Coexistence therefore makes Relay safer** — a benefit the replacement plan would not have captured.

### 4.3 Why the Hub must understand semantics while authorization stays at the ends

The Hub understands **routing / queueing / metering** (the source of both recoverability and commercial capability)
but does **not** understand authorization:

- Commands are signed end-to-end by the client's Ed25519 private key; public keys are registered at the Gateway,
  which verifies the signature locally and adjudicates against its own ACL before executing.
- **Consequence**: a fully compromised Hub lets an attacker read ciphertext (and not even the content if E2E is on),
  drop or replay commands — but **issue no valid new command to the Gateway**.
- This generalises doc 24 OQ-7 ("the relay does not validate user tokens"):
  **"relay holds no secrets" is upgraded to "the hub holds no authorization"**.

### 4.4 Why Relay must be frozen

A **frozen** second mode costs almost nothing to maintain; a **still-evolving** second mode keeps competing with the new
mode for say in protocol design, and in the end both are incomplete. Hence the cancellation of the QUIC transport and
multi-instance Redis sharding previously planned in doc 24. Freezing does **not** mean waiving P1 —
Relay still gets its allowlist gate, once.

---

## 5. Consequences

### 5.1 Positive

- Remote access gains an **explicit recovery point** under restricted networks (cursor resume) instead of
  total stream loss and rebuild
- Offline messages, cross-device sync, push, auditing and metering become possible → the commercial surface exists
- The four most complex pieces of remote-MQTT code are unused in Hub mode; control-plane exposure is eliminated
  by construction (**both modes benefit**)
- Breaks the chicken-and-egg deadlock of doc 24 v0.2.1 ("the tunnel must exist before any remote user can log in"):
  in Hub mode login completes at the Hub, and the session is established only after pairing
- The compliance blocker is **downgraded**: domestic users can immediately use "your own VPS + Relay" with zero
  compliance exposure for us, so the Hub's domestic/overseas topology decision can be deferred without blocking
  Phase 0–2 engineering

### 5.2 Negative, and the price to be paid (stated plainly)

- **The Hub is no longer protocol-agnostic**: every remote capability must be declared and tested.
  This is the single genuine complexity source of this decision
- Commands traverse two extra hops; interactive latency increases (target p50 < 300 ms, to be measured)
- Streaming output requires batch merging: +150–250 ms to first token
- With E2E enabled the Hub cannot search content or preview it in push notifications
- **Long-term maintenance of two modes**: even frozen, Relay still needs security fixes and the P4 dual-transport gate
- Clients must implement two transports (bounded in blast radius by the P3 abstraction)

### 5.3 Risks and required pre-validation

| Risk | Mitigation |
|---|---|
| **A1: SSE buffered by a real reverse proxy / CDN** (the precondition for H2) | First validation item of Phase 0; long polling is **semantically equivalent** by design, differing only in latency → the risk is not fatal |
| Cursor resume cannot guarantee "no loss, no duplication" | **The Phase 1 chaos test is the acceptance criterion for the whole plan**: kill connections at random byte offsets. If it fails, **stop and do not fund Phase 2+** |
| Coexistence rot (P1/P4 lost) | CI drift check + dual-transport conformance suite as **mandatory gates** |
| Projections mistaken for authority (silent wrong state) | Mandatory `source_version` + explicit staleness UI (silent fallback prohibited) |

---

## 6. Implementation Boundary

| Where | What |
|---|---|
| **Open repo** (this ADR / design doc 26) | `acowork-core::hub` wire format (the **only** definition site), the single-tenant self-hostable Hub reference implementation, Gateway `hub-connector`, the client `RemoteClient` abstraction and Hub transport, `RemoteSurfaceManifest` + CI drift check + Relay allowlist generation, E2E envelope encryption mechanism, pairing and device signature verification, the Relay freeze |
| **Closed repo** (the Hub multi-tenant SaaS implementation-layer doc, not distributed with this repo) | Multi-tenant data model (`tenant_id` discriminator, Postgres/Redis tiering), subscriptions and usage metering/billing, geo-admission and fencing, domestic vendor push channels, admin console and audit export, SLA, cross-tenant interop, domestic cloud deployment and the ICP filing-entity decision |

**Hard rules C1–C4 continue to apply**: the closed repo depends on the open `acowork-core` crate and **must not fork it**;
protocol changes flow one way (closed proposes → open publishes → closed follows); commercial capability lives only in
the implementation layer, and `caps` at the protocol layer is declared, never consumed.

---

## 7. Rollback

- Gateway configuration `remote.transport = relay | hub | off`, rolled out **per Gateway**. If Hub misbehaves,
  one switch returns to the doc 24 path and Relay code is untouched; and vice versa.
- The two modules (`src/relay/` and `src/hub/`) run **mutually exclusively** and share only the device-identity and
  ACL adjudication layers; neither may depend on the other, so a failure in one never requires rolling back the other.
- Projections and E2E are Phase 3 increments and can be disabled independently
  (disabling E2E must be an **explicit configuration**, never a silent degradation).

---

## 8. Open Questions

See design doc 26 §14 (HQ-1 … HQ-7). **HQ-1 (SSE buffering behaviour) must be answered in Phase 0**;
HQ-4 and HQ-6 require a joint decision with the closed repo because they touch the open/closed identity-system boundary.
