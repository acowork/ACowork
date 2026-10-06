# ADR-036: MQTT Connection State Pushed by the Backend, Frontend Only Consumes

> **Chinese source of truth**: [ADR-036](../zh/ADR-036-mqtt-status-push.md)
> **Terminology**: see [GLOSSARY.md](./GLOSSARY.md)

## Status

Draft

## Date

2026-07-16

## Decision Makers

大鱼 (Dayu)

## Related

- [ADR-033](./ADR-033-mqtt-replace-grpc-websocket.md) — MQTT replaces gRPC and WebSocket
- [ADR-034](./ADR-034-mqtt-http-boundary.md) — MQTT/HTTP boundary; event topics should carry data
- [ADR-035](./ADR-035-mqtt-streaming-push-refactor.md) — streaming refactor, MQTT direct push

**Correction**: ADR-033 claimed the frontend need not observe MQTT connection state because the
Gateway daemon guarantees it. That is inaccurate — each Agent Runtime process maintains its
own MQTT connection, outside the Gateway daemon, so the frontend MUST see its state.

---

## Context

After a Desktop restart and agent reconnect, session data loads fine over HTTP but the
input box stays stuck on "connecting to agent…". HTTP works, MQTT is down, and the UI
cannot tell. HTTP goes through the Gateway reverse proxy and is independent of MQTT state;
the agent Runtime process was reaped or its socket broke, but nothing surfaces that.

The architectural root cause is a **misplaced source of truth**:

- `chatStore.ts` treats `mqttConnected` as a **one-shot snapshot** — set to `true` the first
  time `initMqttListener` succeeds, never written again.
- `mqtt_client.rs::connect` uses `rumqttc::AsyncClient` and the eventloop handles only
  `Incoming::Publish`; it **never observes `Incoming::ConnAck` or `Incoming::Disconnect`** and
  pushes no status events.
- `chat_mqtt.rs::connect_mqtt` stores the `Arc` in state on success and **emits no disconnect
  event**.
- `AppLayout.tsx:367` carries a comment claiming "ADR-033: MQTT connection is managed by the
  Rust backend — no reconnect", dressing "not implemented yet" up as "not needed", which left
  the `mqttConnected=false` path with no owner.

The dead fields `reconnectAttempts` / `reconnectTimer` (`chatStore.ts:301,303`) show someone
previously opened this path and never finished it.

## Decision

1. **The Rust `rumqttc` eventloop is the source of truth for connection state.** The
   frontend only **consumes** `mqttConnected`, never assigns it.
2. **The Rust eventloop observes `Incoming::ConnAck` / `Incoming::Disconnect`** and pushes
   transitions out through an `on_status` callback.
3. **The frontend receives pushes on a dedicated `mqtt-status` Tauri event**, subscribed once
   for the lifetime of the app. Do not reuse the `agent-event` channel.
4. **The UI MUST be visibly correct when MQTT is down** — `inputDisabled`, error toasts, and the
   status bar all consume `mqttConnected`. A "HTTP fine, MQTT down, UI looks normal" state is
   not acceptable.

### Rust: eventloop to status callback (fully async)

```rust
// mqtt_client.rs
pub enum MqttStatus {
    Connected,
    Disconnected { reason: String },
}

pub async fn connect(
    broker: &str, port: u16, client_id: &str,
    on_publish: impl Fn(MqttMessage) + Send + Sync + 'static,
    on_status:  impl Fn(MqttStatus)  + Send + Sync + 'static,
) -> Result<Self, String> {
    let (client, mut eventloop) = AsyncClient::new(options, 100);

    // connect() returns immediately; transitions arrive asynchronously via on_status.
    // No blocking, no waiting, and no "connecting" intermediate state.
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) =>
                    on_status(MqttStatus::Connected),
                Ok(Event::Incoming(Incoming::Disconnect)) =>
                    on_status(MqttStatus::Disconnected { reason: "broker sent DISCONNECT".into() }),
                Ok(Event::Incoming(Incoming::Publish(p))) =>
                    on_publish(MqttMessage { topic: p.topic.clone(), payload: p.payload.to_vec() }),
                Ok(_) => continue,
                Err(e) => {
                    on_status(MqttStatus::Disconnected { reason: format!("eventloop error: {e}") });
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    Ok(Self { client, _eventloop_guard: Arc::new(EventLoopGuard { _task: poll_task }) })
}
```

**Why not `wait_for_connack` or any synchronous connect wait.** MQTT connection state is
event-driven: `eventloop.poll()` yielding `Incoming::ConnAck` *is* the notification, so
wrapping it in a synchronous wait is wrong. It would turn an async event into a blocking
one (`connect()` could block 10s leaving the Tauri command pending and the frontend
unresponsive when the broker is unreachable), require extra timeout / backoff / error
paths, and introduce a "connecting" intermediate state that degrades the two-state
machine. Instead `connect()` returns immediately, and a separate synchronous
`get_mqtt_status` query supplies the initial state after the frontend registers its listener.

### Tauri: emit `mqtt-status` and keep a `last_mqtt_status` slot

```rust
// commands/chat_mqtt.rs
let last_mqtt_status = state.last_mqtt_status.clone();

let on_status = move |status: MqttStatus| {
    let payload = match &status {
        MqttStatus::Connected => serde_json::json!({ "connected": true }),
        MqttStatus::Disconnected { reason } => serde_json::json!({
            "connected": false, "reason": reason,
        }),
    };
    if let Err(e) = app.emit("mqtt-status", payload) {
        warn!("failed to emit mqtt-status: {e}");
    }
    // Mirror into the shared slot so get_mqtt_status can return the latest value
    // synchronously, without waiting for an event.
    let slot = last_mqtt_status.clone();
    tokio::spawn(async move { *slot.write().await = Some(status); });
};
```

`AppState.last_mqtt_status: Arc<RwLock<Option<MqttStatus>>>` is a **three-state slot**:
`None` means no transition observed yet, and `Some(Connected)` / `Some(Disconnected)`
mean the latest transition. The three states separate "unknown / connected /
disconnected" and close the race where the frontend misses the initial state before its
listener is registered.

### Frontend: subscribe to the event, then query the initial state

```typescript
// stores/chatStore.ts
export async function initMqttListener(): Promise<void> {
  // (1) Subscribe to subsequent transitions.
  _mqttStatusUnlisten = await listen<{ connected: boolean; reason?: string }>(
    "mqtt-status",
    (event) => {
      useChatStore.setState({
        mqttConnected: event.payload.connected,
        lastMqttError: event.payload.connected ? null : event.payload.reason ?? null,
      });
    },
  );

  // (2) Immediately pull the current state after registering the listener — this
  //     closes the window where an event could be lost between the connect_mqtt
  //     return and the listen completing.
  try {
    const snapshot = await invoke<{
      known: boolean; connected: boolean; reason?: string | null;
    }>("get_mqtt_status");
    if (snapshot.known) {
      useChatStore.setState({
        mqttConnected: snapshot.connected,
        lastMqttError: snapshot.connected ? null : snapshot.reason ?? null,
      });
    }
    // snapshot.known === false means the poll task has observed no transition yet;
    // leave the store alone.
  } catch (err) {
    // An older binary lacks the command — degrade to the event stream alone.
    console.warn("get_mqtt_status failed:", err);
  }
}
```

**State machine semantics across Rust and the frontend**

- After startup, `mqttConnected=false, lastMqttError=null`: unknown state, the input
  placeholder reads "connecting to agent…", and **no** yellow warning bar is shown.
- On a `Connected` event, `mqttConnected=true`: the input becomes usable.
- On `Disconnected{reason}`, `mqttConnected=false, lastMqttError="…"`: the input is disabled and
  the status bar shows a yellow warning.
- On `Connected` again, `mqttConnected=true`: the warning clears.

Every existing `mqttConnected` reference in `AppLayout.tsx`, `SplashScreen.tsx`, and
`ChatPanel.tsx` (`inputDisabled`, error toasts, status bar) stays as is — only the source
changes from "guess once at init" to "receive live pushes".

### Remove the dead fields

`reconnectAttempts` / `reconnectTimer` are deleted from `chatStore.ts`. Connection state is
owned by Rust, so the frontend needs no local retry timer.

## Impact

| File | Change |
|------|--------|
| `src-tauri/src/mqtt_client.rs` | `connect` gains an `on_status` parameter; the eventloop handles `ConnAck` / `Disconnect` / `Err`; does **not** wait for ConnAck |
| `src-tauri/src/state.rs` | add `last_mqtt_status: Arc<RwLock<Option<MqttStatus>>>` three-state slot |
| `src-tauri/src/commands/chat_mqtt.rs` | `connect_mqtt` registers the `mqtt-status` emit and mirrors into `last_mqtt_status`; new `get_mqtt_status` command |
| `src-tauri/src/lib.rs` | register `get_mqtt_status` in `invoke_handler` |
| `src/stores/chatStore.ts` | delete `reconnectAttempts` / `reconnectTimer`; `initMqttListener` subscribes to `mqtt-status` and queries `get_mqtt_status` |
| `src/components/layout/AppLayout.tsx` | correct the wrong comment to `ADR-036`; `lastMqttError` guard distinguishes "unknown" from "disconnected" |

Roughly 120 lines (Rust 70 + TS 50), with no architectural breakage.

## Non-goals

- **No waiting for ConnAck in Rust.** Connection state is event-driven; a wait would
  synchronize an async event and add timeout and backoff error paths.
- **No frontend retries.** Reconnection belongs entirely to the Rust eventloop (`rumqttc`
  reconnects internally). The frontend only reflects state.
- **No custom backoff state machine.** The reconnect built into `rumqttc` is sufficient;
  rewriting it reinvents the wheel.
- **No Tauri-layer heartbeat.** MQTT already has keep-alive, and the eventloop detects a
  dropped connection.
- **No change to the ADR-033 transport boundary.** This ADR only closes the connection
  state visibility gap and does not reopen the decision to replace gRPC with MQTT.

## Rejected alternatives

**A — poll Rust from `chatStore.ts` on a `setInterval`.** Rejected: it contradicts the
`rumqttc` design (the connection lifecycle belongs to Rust), adds pointless IPC traffic,
and puts the state source back in the frontend.

**B — have the frontend watch a special `agent-event` topic that self-reports state.**
Rejected: semantic pollution — `agent-event` is the business message channel and connection
metadata does not belong there. A separate `mqtt-status` channel is cleaner.

## Revision history

- **2026-07-16 v2** — removed `wait_for_connack` and the synchronous `on_status(Connected)`:
  synchronous waiting synchronizes an async event and degrades the state machine to three
  states. Now `connect()` returns immediately, a `get_mqtt_status` command supplies the
  synchronous query, and `last_mqtt_status: Option<MqttStatus>` distinguishes "unknown /
  connected / disconnected" so the frontend can pull the current state as soon as its
  listener is registered.

## Verification

1. **Functional** — start the Desktop, kill the agent Runtime (`kill -9`), verify a
   `mqtt-status` event with `connected: false` arrives within ~1s; restart the Runtime and
   verify `connected: true`.
2. **UI** — while disconnected the input is disabled and the status bar shows "connection
   lost"; after reconnect the input returns.
3. **Build** — `cargo clippy --all-targets -- -D warnings` plus `tsc --noEmit`.
