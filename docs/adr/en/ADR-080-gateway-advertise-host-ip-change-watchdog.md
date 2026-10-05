# ADR-080: Gateway advertise-host Drift Self-Healing (if-watch drives pm / doc / embed)

> **Chinese source of truth**: [ADR-080](../zh/ADR-080-gateway-advertise-host-ip-change-watchdog.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Accepted

## Date

2026-09-17

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-055](./ADR-055-remote-runtime-node-topology.md) D3 — Gateway advertise-host resolution
- [ADR-064](./ADR-064-pm-standalone-process.md) — pm/doc standalone processes
- [ADR-073](./ADR-073-agent-instance-identity-decomposition.md) — instance_id identity layering
- Symmetric counterpart: §6.3.3 / Runtime `mqtt/client.rs` already self-heals endpoint changes
  on the Node to Runtime side

---

## 1.1 Scope of impact

`advertise_host` feeds not only `pm_mcp_url` / `doc_mcp_url` but also the embed
process publish URL. All three are the same class of problem, so one watchdog covers
them all.

| Subsystem | Uses advertise_host | Retained topic | Re-publish path | Affected |
|------------|--------------------|-----------------|-----------------|----------|
| `acowork-pm` | yes, `gw.pm_mcp_url` | `acowork/global/mcps` | `MqttPublisherTrigger::trigger()` to `publish_mcps()` | yes |
| `acowork-doc` | yes, `gw.doc_mcp_url` | `acowork/global/mcps` | same trigger, same payload | yes |
| `acowork-embed` | yes, `format!("http://{}:{}/v1", gw.advertise_host, eps.port)` (`mqtt/global_resources_builders.rs:267`) | `acowork/global/embedding_models` | same trigger to `publish_embedding_models()` | yes |
| `acowork-lsp-relay` | no — spawned by Node, bound to `127.0.0.1` | no — Node control plane | — | no |
| cloud embedding | no — `active_base_url` comes from provider config | yes, but unrelated endpoint | — | no |

Key reuse: `mqtt/global_resources_publisher.rs::LoopHelper::publish_all()` publishes all
five topics (providers / mcps / searches / embedding_models / user_profiles) at
once, so a single `trigger()` resynchronizes pm, doc, and embed together.

## 1. Problem

`Gateway::run` probes the LAN IP once at startup via
`config::resolve_advertise_host()`, bakes it into `pm_mcp_url` / `doc_mcp_url`, and
publishes it as retained `acowork/global/mcps`, which each Runtime persists to
`agent_mcp.json`.

**After the user switches Wi-Fi, VPN, or router, the host IP changes but the Gateway
process does not restart.** `advertise_host` still points at the old IP, every Runtime

`agent_mcp.json` holds a dead pm/doc MCP URL, and all `mcp_pm__*` calls fail
(transient `HTTP request to MCP server failed`). The only recovery is a manual Gateway
restart. §6.3.3 already fixed the mirror case on the Runtime side; **the Gateway side
had no symmetric mechanism.**

## 2. Decision

Add a background watchdog task inside the Gateway that subscribes to OS-level
interface address change events (Windows `NotifyAddrChange` / Linux `NETLINK_ROUTE` /
macOS `SCDynamicStore`), wrapped by the `if-watch` crate (v3, tokio feature). On each
`IfEvent::Up` / `IfEvent::Down`, re-run the `detect_non_loopback_ip()` UDP-trick
detector and compare against the current `gw.advertise_host`:

- **Same** — no-op, avoiding a pointless republish.
- **Different** — atomically update `gw.advertise_host`, rebuild `pm_mcp_url` /
  `doc_mcp_url`, then call `MqttPublisherTrigger::trigger()` to republish
  `acowork/global/mcps`. Every subscribed Runtime receives the new retained message
  and persists it to `agent_mcp.json`.

**Always started**: the watchdog starts even when the operator pinned `advertise_host`
explicitly via `--advertise-host` or `gateway.toml`. A pin sets the **initial value**
only and cannot prevent later IP drift (a Wi-Fi/VPN/router change invalidates a pinned
IP anyway). `reconcile()` rewrites `gw.advertise_host` only when the detected IP
**differs**, so a pinned-but-still-valid address is never touched; only a genuinely dead
address self-heals.

## 3. Design points

**3.1 Do not parse the event payload — re-run the detector.** An event means "the
network changed", but the judgement of *which* IP is the best externally
reachable one already lives in `detect_non_loopback_ip()` (the UDP connect to
1.1.1.1 trick picks the primary route egress). Re-running it is simpler, more
robust, and reuses existing logic.

**3.2 Lock granularity.** `reconcile()` takes the `GatewayState` write lock only on the
state-change path and explicitly `drop(gw)` before `trigger()`, never holding a
lock across `notify_one()`.

**3.3 An explicit pin is an initial value, not a switch.** `reconcile()` rewrites
`gw.advertise_host` and republishes whenever the detected IP differs; a pinned and
still-valid address stays untouched.

**3.4 Zero Runtime changes.** `mqtt/client.rs::handle_global_mcps` already listens for the
retained `acowork/global/mcps` message and persists to `agent_mcp.json`, so this is a
**pure Gateway-side fix with no Runtime crate change** — it validates the existing
retained-channel contract.

**3.5 Event API compatibility.** Under its `tokio` feature `if-watch` re-exports
`IfWatcher` (on Windows `if_watch::tokio::IfWatcher` is an alias of
`win::tokio::IfWatcher`; Linux likewise). `IfWatcher::new()` returns
`Result<Self, std::io::Error>` — **synchronous, not a future** — and implements
`Stream<Item = Result<IfEvent, std::io::Error>>`, consumed via
`tokio_stream::StreamExt::next()`.

A new `if_watch_api_compiles` unit test acts as an **upstream API drift gate**: any
breaking change upstream fails compilation, so CI catches it.

## 4. Alternatives

| Option | Upside | Downside | Adopted |
|--------|--------|---------|---------|
| A. Periodic polling of `detect_non_loopback_ip` | simplest, no new dependency | granularity is a trade-off (fast wastes resources, slow adds latency), and it is blind to events the OS already knows about | no |
| B. if-watch event driven (this ADR) | native cross-platform API, zero latency, no CPU waste | one more crate (~10 KB of build output) | **yes** |
| C. Hand-written per-platform syscalls | zero dependencies | 150+ lines of platform-specific code and a complex build matrix | no |
| D. Replace the IP with hostname / mDNS | solves multi-host and container drift in one move | needs consistent DNS; mDNS is unavailable on some networks; far wider blast radius than this bug | no — a separate ADR |
| E. Add a 5-minute fallback recompute | belt-and-suspenders | the event-driven path is already real-time; a fallback is over-engineering that masks design problems | no — cut |

## 5. Impact

**Added**

- `core/acowork-gateway/src/lifecycle/advertise_watchdog.rs` — event loop plus `reconcile`
- `core/acowork-gateway/Cargo.toml` — `if-watch = { version = "3", features = ["tokio"] }`

**Modified**

- `config.rs` — `detect_non_loopback_ip` becomes `pub(crate) fn`
- `lifecycle/mod.rs` — register the new module
- `gateway/mod.rs` — spawn the watchdog after `MqttPublisherTrigger` is created and before returning `Some(trigger)`

**Unchanged**

- The Runtime crate (`core/acowork-runtime/`) — the retained subscriptions already exist in
  `available_cache.rs` / `mqtt/client.rs`
- The Desktop app
- The PM / Doc subprocesses — they talk to the Gateway over `127.0.0.1:{port}` and never see the WAN IP
- Node Agent and the LSP relay — Node spawns them and binds loopback

## 6. Tests

| Test | Location | Covers |
|------|----------|--------|
| `if_watch_api_compiles` | `lifecycle/advertise_watchdog.rs` | upstream API drift gate |
| `reconcile_keeps_state_when_no_ip_detected` | same | the early-return path when no IP is found |
| `test_build_available_mcps_*` (4 existing) | `mqtt/global_resources_builders.rs` | the `build_available_mcps` contract using `gw.pm_mcp_url`, unbroken by this change |
| Full gateway suite (478 tests) | whole crate | no regression — `cargo test -p acowork-gateway --lib` all green |
| **Manual / on-machine** (pending) | run a debug gateway, switch Wi-Fi, confirm `agent_mcp.json` updates | end to end |

## 7. Known limits / follow-ups

- **IPv6** — `detect_non_loopback_ip` only inspects IPv4, so an IPv6-only change never
  triggers an update. Extend the detector if IPv6 becomes the dominant LAN protocol;
  short term, home and dev networks remain IPv4.
- **Container / cross-host drift** — this ADR covers a single host and single process. Drift
  across a multi-host or orchestration layer (k8s service, Docker network) needs a
  separate discovery mechanism (mDNS, Consul) and is out of scope.
- **A first event may be missed** — the Windows `if-watch` implementation can miss events at the
  instant of startup. The fix is a first `reconcile` fired immediately after the watchdog
  starts; not yet done, and available as a small follow-up.
