# ADR-065: Unifying the MQTT Client Lifecycle Across All Four Ends

> **Chinese source of truth**: [ADR-065](../zh/ADR-065-unify-mqtt-client-lifecycle.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Decided (2026-09-03), implemented

## Date

2026-09-03

## Decision Makers

大鱼 (Dayu) — finalized by the architecture review

## Related

- [ADR-039](./ADR-039-mqtt-client-lifecycle.md) (the MQTT client lifecycle framework — the
  evolution target of this ADR)
- [ADR-055](../zh/ADR-055-remote-runtime-node-topology.md) (Node Agent topology — the reason the Node
  MQTT client exists)
- [ADR-036](./ADR-036-mqtt-status-push.md) (MQTT connection state pushed by the backend)
- [docs/protocols/zh/mqtt.md](../../protocols/zh/mqtt.md) (the MQTT protocol reference)

---

## 1. Decision Summary

Consolidate the **complete lifecycle of the four MQTT clients (Desktop / Node / Runtime / Gateway
publisher)** — the poll loop, error classification, backoff reconnect, soft-restart, wake recovery,
and timing parameters — into the shared **`acowork-mqtt-session` crate**, leaving only the
entity-level differences (client_id / LastWill / topic prefixes / bootstrap steps / publish
payloads) in each end.

This eliminates in one pass every homologous defect exposed by the "Node Agent goes silent for 60
seconds after wake" incident:

| Defect | Current state | Consequence |
|--------|---------------|-------------|
| Four independently written error adapters | Node / Gateway omitted the `MqttState::Io` unwrap → a wake reset was misclassified as E4 fatal | the Node took a 60s fatal backoff after wake; start/stop commands were silently lost |
| Timing parameters inconsistent across ends | keepalive 5s/5s/30s/-; watchdog 5s/5s/60s/- | behavioural drift, impossible to tune uniformly |
| Wake recovery inconsistent across three ends | Desktop 2s + Focused / Node 5s / Runtime none | the same wake event yielded 370ms vs 60s recovery |
| `force_reconnect` API inconsistent | AtomicBool+Notify / Notify only / none | a lost-notify race exists |

---

## 2. Context and Root Cause

### 2.1 The failure chain (user-reported 2026-09-03)

The user powered on the machine (OS wake) and tapped start on senior-engineer / document-manager
with no response; recovery came automatically about 60 seconds later. Evidence from all four ends:

```text
08:16:14.551  broker: disconnected desktop  error=Network(KeepAlive(Elapsed(())))
08:16:14.553  broker: disconnected gateway  error=Network(KeepAlive(Elapsed(())))
08:16:14.554  broker: disconnected node     error=Network(KeepAlive(Elapsed(())))
              ↑ at the wake instant the three processes were frozen, PINGREQ was not sent in
                time, and the broker evicted them on keepalive timeout

08:16:14.552  Desktop: Actual system sleep detected sleep_ms=12953
08:16:14.668  Desktop: MQTT force-restart requested during fatal backoff
08:16:14.922  Desktop: MQTT reconnected after wake          ← 370ms recovery
08:16:14.556  Node:    MQTT event loop error err_class="E4 ConfigError"
08:17:14.560  Node:    MQTT (re)connected                    ← 60s recovery
```

### 2.2 Root cause: the Node's error classifier omitted the `MqttState::Io` unwrap

On wake, rumqttc wraps the TCP reset as `ConnectionError::MqttState(StateError::Io(ECONNRESET))`.
The four ends handle it differently:

| End | Adapter | Result |
|-----|---------|--------|
| **Runtime** | the shared `ErrorDescriptor::from(&e)` (`err_class.rs:213`, already unwraps) | ✅ Transient, exponential backoff |
| **Desktop** | private `error_descriptor_from_rumqttc_025` (`mqtt_client.rs:214`, already unwraps) | ✅ Transient, exponential backoff |
| **Node** | private `error_descriptor_from_rumqttc` (`control/mqtt.rs:95`, **does not unwrap**) | ❌ `ErrorKind::MqttState` → E4 ConfigError (fatal) → **60s fatal backoff** |
| **Gateway** | private adapter (`mqtt/client.rs:59`, **does not unwrap**) | ❌ same (the Gateway publisher has no wake recovery, so the impact is smaller but homologous) |

The correct implementation **already exists in the shared crate** (`From<&ConnectionError>` plus two
regression tests), but all four callers wrote private adapters and only the Runtime used the shared
one.

### 2.3 Timing parameter drift

| Parameter | Desktop | Node | Runtime | Unified value |
|-----------|---------|------|---------|---------------|
| keepalive | 5s | 5s | 30s | **5s** (the broker's `connection_timeout_ms` is 5s) |
| POLL_WATCHDOG | 5s | 5s | 60s | **5s** (1× keepalive). The Runtime's 60s existed to avoid false triggers from long HTTP handlers; the correct fix is "watchdog 5s + actively feed PINGREQ during long tasks", not to loosen the watchdog |
| power probe | 2s | 5s | none | **2s** (must be earlier than the 5s wake threshold, so `detect_resume` returns true within 4s of the wake) |
| wake threshold | 5s | 5s | none | **5s** |
| fatal backoff | 60s interruptible | 60s interruptible | immediate break | **60s interruptible** (`interruptible_backoff`) |

### 2.4 Wake recovery is inconsistent across three ends

| End | Trigger | Recovery path |
|-----|---------|--------------|
| Desktop | 2s polling + a `Focused(true)` window event | `recover_after_wake()` → force_restart → soft-restart |
| Node | 5s polling (`power_tick`) | `force_reconnect()` → Notify → soft-restart |
| Runtime | **none** | none (relies on the 60s watchdog or a parent-process restart) |
| Gateway | none | none |

The Desktop's `ForceRestart` is **AtomicBool + Notify** (guards against a lost notify); the Node has
only **Notify**, which has a lost-notify race outside `select!`.

### 2.5 The Runtime's never-sleep / standalone modes (this ADR must cover them)

The Runtime has two "process stays resident with no parent process to fall back on" modes, in which
**system sleep still freezes the process and drops the MQTT connection, yet the Runtime has no
self-recovery**:

| Mode | Trigger | MQTT recovery after wake depends on |
|------|---------|------------------------------------|
| **never-sleep** | `idle_timeout_secs = 0` (`idle_watcher.rs:85` `NEVER_SLEEP`, the UI "Never" option) | ① the 60s watchdog (far too slow) ② a Node restart — but the Runtime never exited so the Node will not restart it → **in practice no recovery** |
| **standalone** | running independently without a Gateway / Node (`loop_.rs:2008` `test_agent_loop_without_gateway_client`) | no parent process → **entirely dependent on itself** |

Conclusion: **the Runtime must enable the power probe + `force_reconnect`**, on equal footing with
the Desktop and Node. The earlier assumption that "the Runtime is a per-session process and waking is
the parent process's responsibility" **does not hold** — in never-sleep mode the Runtime is a
resident process and wake recovery can only come from itself.

---

## 3. Goals

1. **Single implementation**: the poll loop, error classification, backoff, soft-restart, and wake
   recovery of all four clients converge into `acowork-mqtt-session`; each end writes only its
   entity differences.
2. **Single adapter**: error classification is forced through the shared
   `From<&ConnectionError>`; per-end private `error_descriptor_from_rumqttc` is forbidden.
3. **Single timing**: keepalive / watchdog / power probe / wake threshold / fatal backoff are all
   shared crate constants that no end may override.
4. **Unified wake recovery**: the processes that need it (Desktop / Node / **Runtime**) uniformly
   use `power::run_power_probe_loop` at a 2s interval.
5. **Unified `force_reconnect`**: AtomicBool + Notify semantics, eliminating the lost-notify race.
6. **Behavioural alignment**: for the same OS wake event, recovery time across the four ends
   converges to the same magnitude (< 5s).

## 4. Options

### Option A: the shared crate provides a complete `MqttClient<B>` (chosen)

`acowork-mqtt-session` gains `client.rs` providing a generic, complete MQTT client:

```rust
pub struct MqttClient<B: BootstrapAction> {
    shared_handle: Arc<Mutex<AsyncClient>>,
    state: SessionStateTx,
    reconnect: ReconnectPolicy,
    force_restart: ForceRestart,
    _task: JoinHandle<()>,
}

impl<B: BootstrapAction> MqttClient<B> {
    pub async fn connect(config: MqttClientConfig, bootstrap: B,
                         message_callback: MessageCallback) -> Result<Self, Error>;
    pub fn shared_handle(&self) -> Arc<Mutex<AsyncClient>>;
    pub async fn publish_raw(&self, topic: &str, payload: Vec<u8>,
                             qos: QoS, retain: bool) -> Result<()>;
    pub fn force_reconnect(&self);
    pub fn state_rx(&self) -> SessionStateRx;
    pub fn current_state(&self) -> SessionState;
}
```

- The internal poll loop (soft_restart / classify / backoff / watchdog / force_restart) is
  **written once**
- All timing constants come from the shared crate
- Each end only implements the `BootstrapAction` trait + its entity config

**Pros**: eliminates duplication entirely; behaviour is inherently consistent; future tuning changes
one place. **Cons**: a large one-shot change surface (~250–700 poll lines deleted per end).

### Option B: unify only the error adapter + timing constants (minimal fix)

Force `From<&ConnectionError>` and hoist the timing constants, but keep each end's poll loop.

**Pros**: ~200 lines, landable in a day. **Cons**: four poll loops remain and will drift again; the
wake recovery mechanism stays inconsistent across three ends.

### Option C: fix the Node only

Just add the `MqttState::Io` unwrap to the Node's `error_descriptor_from_rumqttc`.

**Pros**: ~10 lines. **Cons**: treats the symptom, not the cause; the homologous Gateway bug
remains; timing and wake mechanisms stay inconsistent; it will inevitably drift again.

### Decision

**Option A**, because:

- The user explicitly required "extract the correct common trait, reuse it across ends, align the
  timing parameters, no arbitrary per-end behaviour"
- This is the natural endpoint of ADR-039's shared-crate direction: ADR-039 only extracted the state
  machine / classifier / backoff policy and **did not extract the poll loop itself**, which is
  exactly why the four ends drifted again when each implemented its own poll
- Option B can land first as Step 1 of A, but the final form must be A

---

## 5. Detailed Design

### 5.1 New modules in the shared crate

```
core/acowork-mqtt-session/src/
  client.rs        # MqttClient<B> full lifecycle (new)
  config.rs        # MqttClientConfig + timing constants (new)
  force_restart.rs # ForceRestart: AtomicBool + Notify (new)
  power.rs         # detect_resume + run_power_probe_loop (new, extracted from Desktop/Node)
  err_class.rs     # kept; From<&ConnectionError> is the only adapter
  reconnect.rs     # kept
  session.rs       # kept
  session_state.rs # kept
  bootstrap.rs     # kept
```

### 5.2 Timing constants (single source of truth)

```rust
// core/acowork-mqtt-session/src/config.rs
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const POLL_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(5);
pub const POWER_PROBE_INTERVAL: Duration = Duration::from_secs(2);
pub const WAKE_DETECT_THRESHOLD: Duration = Duration::from_secs(5);
pub const FATAL_BACKOFF: Duration = Duration::from_secs(60);
pub const FATAL_STREAK_LIMIT: u32 = 3;
```

**The Runtime's keepalive 30s / watchdog 60s must go back to 5s/5s.** They existed to avoid false
triggers from long HTTP handlers (`POST /workspaces` can exceed 4s); the correct fix is "keep the
watchdog at 5s and feed PINGREQ during long tasks", not loosening the watchdog to 60s (which would
also make wake recovery 60s).

### 5.3 `ForceRestart` (unified semantics)

```rust
pub struct ForceRestart {
    notify: tokio::sync::Notify,
    persistent: AtomicBool, // persists across notified() so the permit is not lost
}

impl ForceRestart {
    pub fn request(&self);      // persistent=true + notify_one
    pub fn take(&self) -> bool; // atomically consume persistent
    pub async fn wait(&self);   // notified()
}
```

The poll loop calls `take()` at the top of `select!` to check the persistent flag before awaiting
`notified()`, covering the window where the poll is busy handling an event and misses the notify.

### 5.4 The `power` module (extracted and merged from Desktop/Node)

```rust
// core/acowork-mqtt-session/src/power.rs
pub fn detect_resume() -> bool;  // merges the Desktop lib.rs and Node power.rs platform impls

pub async fn run_power_probe_loop(
    force_restart: ForceRestart,
    interval: Duration,   // uniformly passed POWER_PROBE_INTERVAL
    label: &'static str,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if detect_resume() {
            tracing::warn!(label, "System sleep/wake detected — forcing reconnect");
            force_restart.request();
        }
    }
}
```

- The Desktop keeps its extra `Focused(true)` window-event trigger (a fast path), but routes
  through `ForceRestart::request()` underneath
- **The Runtime must enable it**: under never-sleep / standalone the Runtime is a resident process
  and can only self-recover (§2.5). When an MQTT client exists at startup, the Runtime also starts
  `run_power_probe_loop`
- The Gateway publisher does not enable the power probe (the Gateway is resident and needs no wake
  recovery; it can be added later on demand)

### 5.5 The final shape of each end after migration

| End | Keeps | Deletes |
|-----|-------|---------|
| **Desktop** | `ALL_TOPIC_FILTERS`, the publish API, Tauri command integration, the `Focused(true)` handler | the entire poll loop in `mqtt_client.rs` (~600 lines), the private `error_descriptor_from_rumqttc_025`, a private `ForceRestart`, the inline `mod power` in `lib.rs` |
| **Node** | the public `force_reconnect` API, the credentials slot, bootstrap registration, the LWT | the entire poll loop in `control/mqtt.rs` (~250 lines), the private adapter, `power.rs` |
| **Runtime** | `run_bootstrap`, the publish API, LWT setup, `BootstrapData`, **starting `run_power_probe_loop` (never-sleep / standalone self-recovery)** | the entire poll loop in `client.rs` (~700 lines), the private adapter |
| **Gateway publisher** | the subscribe/publish business | the private adapter in `mqtt/client.rs` (switch to the shared `From`) |

### 5.6 Hard constraints

- Per-end private `error_descriptor_from_rumqttc` is **forbidden**: the shared crate provides
  `From<&ConnectionError>` and each end writes only
  `classify_err(&ErrorDescriptor::from(&e))`
- Per-end overriding of timing constants is **forbidden**: `MqttClientConfig` exposes only entity
  fields (client_id / host / port / credentials / last_will / max_packet_size); timing fields are
  not exposed
- CI gains a clippy lint: the literal `ErrorKind::MqttState` may not appear outside the shared crate

## 6. Implementation Steps

| Step | Content | Size |
|------|---------|------|
| **Step 1** | extract `power.rs` (detect_resume + run_power_probe_loop) into the shared crate; Desktop/Node switch to it; the probe interval unifies at 2s | ~200 lines |
| **Step 2** | extract `ForceRestart` (AtomicBool + Notify); Desktop/Node switch to it | ~100 lines |
| **Step 3** | add `MqttClient<B>` + `MqttClientConfig` + timing constants; internalize the whole poll loop | ~400 lines |
| **Step 4** | migrate the four ends: delete each poll loop, use `MqttClient<B>`; the Runtime's keepalive/watchdog go back to 5s/5s | 250–700 lines deleted per end |
| **Step 5** | regression tests + CI hard constraints | see §7 |

## 7. Acceptance Criteria

| # | Criterion | Status |
|---|-----------|--------|
| 1 | No private `error_descriptor_from_rumqttc` outside `acowork-mqtt-session` (enforced by a clippy lint) | ✅ the red line `dev/ci.sh::run_mqtt_redline` is in place; **after the Step 4-B closure all four ends are clean** |
| 2 | The four ends cannot override `MqttClientConfig` timing fields (compile-time) | ✅ `MqttClientConfig` holds only entity fields; all timing values are `pub const` and not overridable at construction |
| 3 | `ConnectionError::MqttState(StateError::Io(ECONNRESET))` must classify as Transient (regression test covering the Node/Gateway path) | ✅ added `mqtt_state_io_econnreset_classified_transient_node_gateway_path` |
| 4 | A simulated 12s sleep (clock mocking) → `detect_resume() == true` + `force_reconnect` fires (regression test) | ✅ extracted the pure function `is_resume_gap(prev_biased, prev_unbiased, biased, unbiased) -> bool` with 6 unit tests; `detect_resume()` is now a 4-line wrapper; the `run_power_probe_loop` → `on_resume` → `ForceRestart::request` chain is covered by `request_idempotent` / `wait_resolves_when_requested_while_parked` / `interruptible_backoff_returns_true_on_request` |
| 5 | `interruptible_backoff`: a notify firing during the sleep must return immediately | ✅ Step 2 added 6 `ForceRestart` unit tests, no regression this round |
| 6 | A real OS wake: Desktop / Node / **Runtime (never-sleep mode)** all recover in < 5s (manual) | ⚠️ must be verified manually on a real OS (not part of CI) |
| 7 | `cargo test --all` + `cargo clippy --all-targets -- -D warnings` + `dev/ci.sh all` all green | ⚠️ the Step 5 scope is fully green; the workspace-wide green run is gated by the Step 4 residue below |

### 7.1 What Step 5 closure found, and the Step 4-B closure (2026-09)

**The residue Step 4 left for Step 5**: the red line (`dev/ci.sh::run_mqtt_redline`) executed exactly
as designed in ADR-065 §7 #1; Step 4 in fact only migrated Gateway / Node / Runtime, and **the
Desktop migration was incomplete**.

```
Checking MQTT ErrorKind::MqttState red line (ADR-065 §7 #1)...
ERROR: ErrorKind::MqttState literal found outside acowork-mqtt-session (ADR-065 #1):
apps/acowork-desktop/src-tauri/src/mqtt_client.rs:268:   kind: ErrorKind::MqttState,
apps/acowork-desktop/src-tauri/src/mqtt_client.rs:274:   kind: ErrorKind::MqttState,
apps/acowork-desktop/src-tauri/src/mqtt_client.rs:540:   let desc = error_descriptor_from_rumqttc_025(&e);
apps/acowork-desktop/src-tauri/src/mqtt_client.rs:216:   fn error_descriptor_from_rumqttc_025(err: &rumqttc::ConnectionError)
```

| End | Migrated to `MqttClient<B>` | Private `error_descriptor_from_rumqttc` | `ErrorKind::MqttState` literals |
|-----|----------------------------|--------------------------------------|-------------------------------|
| Gateway | ✅ | 0 | 0 |
| Node | ✅ | 0 | 0 |
| Runtime | ✅ | 0 | 0 |
| **Desktop** | ❌ | **1** (`mqtt_client.rs:216`) | **2** (`mqtt_client.rs:268,274`) |

The Desktop's behaviour is logically equivalent to "shared `MqttClient<B>` + shared
`From<&ConnectionError>`", but it **still routes through a private adapter** rather than the
`MqttClientHandler` trait required by §5.6.

**Step 4-B closure** (the user authorized handling this directly inside Step 5): migrate the Desktop
to the shared `MqttClient<B>` with no behaviour loss.

1. **Audit conclusion**: all 13 public methods of `DesktopMqttClient` are expressible on
   `MqttClient<B>` + the `MqttClientHandler` trait with **no functional gap** — direct equivalents
   for `force_reconnect` / `recover_after_wake` (`reset_to_connecting`) / `wait_for_connected` /
   `current_state` (`session_state`) / `publish_raw` / `shared_handle` (`inner`); the
   `ALL_TOPIC_FILTERS` subscriptions move into `DesktopHandler::on_connack`; the
   `MqttStatus` → Tauri `mqtt-status` state bridging moves into
   `on_publish`/`on_disconnect`/`on_error`/`on_soft_restart`.
2. **Behaviour parity**: keepalive 5s / watchdog 5s / fatal-streak 3 / fatal-backoff 60s /
   clean-session true / queue 100 / packet size `GATEWAY_MQTT_MAX_PACKET_SIZE` /
   resubscribe-on-ConnAck are all preserved via the shared constants + `MqttClientConfig`.
3. **Deleted code**: the private `ForceRestart` (47 lines), the private
   `interruptible_backoff` (22 lines), the private `error_descriptor_from_rumqttc_025` (94 lines),
   the private `resubscribe_all` (11 lines), the 250+ line inline poll task (replaced by a single
   `MqttClient::connect` call), and the dead code `subscribe_agent_session` /
   `unsubscribe_agent_session` (never called by the frontend; the corresponding
   `mqtt_subscribe_agent_session` / `mqtt_unsubscribe_agent_session` Tauri commands in
   `chat_mqtt.rs` were deleted too).
4. **New structure**: `DesktopHandler` (73 lines) carrying only the entity differences —
   `on_publish` / `on_connack` (re-subscribe `ALL_TOPIC_FILTERS` + Connected) / `on_disconnect`
   (Reconnecting{reason}) / `on_error` (Reconnecting{reason}) / `on_soft_restart` (Connecting) —
   and `DesktopMqttClient` as a thin `#[derive(Clone)]` wrapper over
   `MqttClient<DesktopHandler>`.

**Result**:

| Metric | Before | After |
|--------|--------|-------|
| `mqtt_client.rs` lines | 944 | **487** (-48%, 457 inline lines deleted) |
| Desktop `ErrorKind::MqttState` literals | 2 | **0** |
| Desktop private `error_descriptor_from_rumqttc` | 1 | **0** |
| Desktop private `ForceRestart` | 1 | **0** |
| Desktop private `interruptible_backoff` | 1 | **0** |
| Desktop private `resubscribe_all` | 1 | **0** |
| desktop `cargo check --lib` | ✅ | ✅ (no new warnings) |
| desktop `cargo clippy --lib` on `mqtt_client.rs` | n/a | ✅ (0 warnings) |
| desktop `cargo test --lib` | 23 | **23** (all pass) |
| `dev/ci.sh::run_mqtt_redline` | ❌ | **✅** |
| `acowork-mqtt-session` tests | 57 | **57** (no regression) |
| `acowork-node` tests | 117 | **117** (no regression) |

**The `on_status` callback semantics in `chat_mqtt.rs` were untouched** — the mapping is perfect:
the three `MqttStatus` variants (`Connected` / `Connecting` / `Reconnecting { reason }`) correspond 1:1
with the original code and the Tauri event payload fields (`connected` / `connecting` /
`reconnecting` / `reason`) are emitted unchanged. The original `MqttStatus::Disconnected { reason }`
was dead code; after its removal the `match` in the `connect_mqtt` Tauri command was slimmed down
accordingly.

**`Cargo.toml` change**: the private `DesktopMqttClientError` (derived with `thiserror::Error`) was
never used because `connect` returns `Result<Self, String>` — deleted outright, no new dependency.
Before this task landed, `dev/ci.sh all` was blocked by the red line, but **this is the expected
behaviour** — the red line exists precisely to catch this class of regression.

### 7.2 Step 5 incremental unit test statistics

| Module | Before Step 5 | After Step 5 | Added |
|--------|---------------|--------------|-------|
| `acowork-mqtt-session` (lib) | 41 | **57** | +16 (`is_resume_gap` × 6, `mqtt_state_io_econnreset_classified_transient_node_gateway_path` × 1) |

Whole workspace (excluding the two known environment issues: the `onnxruntime.lib` link error in
`acowork-embed` and the external LSP binary required by `acowork-lsp-relay`): **2477 tests,
0 failed**.

## 8. Rollback

- **Steps 1/2** (power / ForceRestart extraction): a pure refactor with equivalent behaviour; a
  `git revert` suffices
- **Steps 3/4** (`MqttClient` consolidation): the git history of each end's poll loop is preserved,
  so if the shared client causes problems it is possible to fall back to the intermediate "each end
  keeps its own poll + the shared adapter" form (i.e. Option B)
- **Timing parameters**: if the Runtime's keepalive 30s→5s causes long HTTP handlers to disconnect
  spuriously, the fallback is "watchdog 5s + actively feed PINGREQ during long tasks", **not**
  restoring the 60s watchdog

## 9. Decision Record

| Decision point | Outcome |
|----------------|---------|
| Consolidation scope | the **complete lifecycle** (poll / classification / backoff / soft-restart / wake recovery / timing) moves into the shared crate |
| Error adapter | the shared `From<&ConnectionError>` is the only implementation; private adapters are forbidden |
| Timing parameters | shared crate constants that no end may override; the Runtime's keepalive/watchdog go back to 5s/5s |
| Wake recovery | Desktop / Node / **Runtime** uniformly use `power::run_power_probe_loop` (2s); the Gateway does not enable it |
| `force_reconnect` | unified AtomicBool + Notify semantics |
| Runtime long tasks | the watchdog stays at 5s; feed PINGREQ during long tasks (do not loosen the watchdog) |
| Runtime never-sleep / standalone | **must enable the power probe for self-recovery** (a resident process can only wake-recover by itself) |
| Implementation path | Option A (full consolidation), landed in Steps 1–5, with Steps 1/2 shippable independently |
