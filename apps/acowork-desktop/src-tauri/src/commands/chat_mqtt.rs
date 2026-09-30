//! Tauri commands for MQTT operations (ADR-033 Phase 3)
//!
//! These commands are called from the React frontend via `invoke()`:
//! - `connect_mqtt` — connect to the MQTT broker
//! - `disconnect_mqtt` — disconnect and clean up
//! - `force_reconnect_mqtt` — force a fresh event loop after a wake
//! - `get_mqtt_status` — read the broker session state
//!
//! Every user-initiated session action (chat, stop, config, lifecycle,
//! approval, etc.) now goes through the Gateway's authenticated HTTP API
//! (`src/lib/session-control.ts`). The MQTT control plane is empty in
//! the Desktop direction since auto-sleep was retired in Sept 2026 —
//! there is no `ActiveHeartbeat` to send.
//!
//! The `connect_mqtt` message callback decodes the agent-status topic
//! (`acowork/agents/+/status`, protobuf `DataEnvelope<AgentStatus>`)
//! plus DevMode debug events (ADR-048 D6) on
//! `acowork/agents/glm-5.3_common/debug/events/#` and re-emits them on
//! the `agent-event` / `debug-event` Tauri channels for the frontend
//! stores.

use std::sync::Arc;

use prost::Message;
use tauri::Emitter;

use acowork_core::mqtt_proto::{
    BootstrapState, DataEnvelope,
    data_envelope, session_message,
};
use crate::mqtt_client::{DesktopMqttClient, MqttMessage, MqttStatus};
use crate::state::{AppState, BootstrapStateView};
use acowork_core::defaults;

/// Connect to the MQTT broker and start receiving events.
///
/// Called by the frontend after the Gateway is confirmed healthy.
/// Subscribes to agent lifecycle topics and starts forwarding events
/// to the frontend via `app.emit("mqtt-event", payload)`.
///
/// ADR-036: also wires the broker eventloop's CONNACK / DISCONNECT
/// transitions to a dedicated `mqtt-status` Tauri event so the React
/// `chatStore` can keep `mqttConnected` truthful after a Desktop
/// restart or Runtime process recycling.
#[tauri::command]
pub async fn connect_mqtt(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.mqtt_client.lock().await;

    let user_id = "default"; // Single-user phase; multi-user will use actual user_id

    // ADR-058 W4 + ADR-055 D3: derive both the MQTT broker host AND port
    // from the Gateway so Remote mode (Gateway behind an SSH tunnel / WSL
    // IP) reaches the broker through the same forwarded host as :19876
    // HTTP. The host is derived from the base URL; the port is fetched
    // dynamically from /api/status (L3-6 residual gap — ADR-058 W4 fixed
    // the host half, ADR-055 Phase 1.3 closes the port half). Local mode
    // derives "127.0.0.1" — identical to the previous hardcode.
    let (mqtt_host, mqtt_port, mqtt_credentials) = {
        let gw = state.gateway.read().await;
        let gateway_base_url = gw.base_url().to_string();
        let mqtt_host = derive_mqtt_broker_host(&gateway_base_url)
            .unwrap_or_else(|| defaults::GATEWAY_MQTT_HOST.to_string());
        // Fetch broker discovery info dynamically; fall back to defaults
        // on any error so the connection still attempts the canonical
        // port. ADR-055 Phase 5a: `mqtt_username` / `mqtt_password` are
        // present only when `mqtt.auth_enabled` is on — None keeps the
        // anonymous connection to an auth-disabled broker.
        let status = gw.system_status().await.ok();
        let mqtt_port = status
            .as_ref()
            .map(|s| s.mqtt_port)
            .unwrap_or(defaults::GATEWAY_MQTT_PORT);
        let credentials = status
            .as_ref()
            .and_then(|s| s.mqtt_username.clone().zip(s.mqtt_password.clone()));
        (mqtt_host, mqtt_port, credentials)
    };
    let mqtt_credentials = mqtt_credentials
        .as_ref()
        .map(|(u, p)| (u.as_str(), p.as_str()));

    // Idempotence guard: reuse the existing client only when it targets
    // the same broker endpoint as the current configuration. A stale
    // client — created before the user edited the remote Gateway address
    // on the SplashScreen timeout view or in Settings — would otherwise
    // keep publishing every control command (chat messages, session
    // management) to the OLD Gateway's broker with no visible error,
    // while all agents live on the new one. Tear it down so the rebuild
    // below targets the configured broker.
    if guard.is_some() {
        let current_endpoint = state.mqtt_endpoint.lock().await.clone();
        if endpoint_matches(current_endpoint.as_ref(), &mqtt_host, mqtt_port) {
            return Ok(()); // Already connected to the configured broker
        }
        tracing::info!(
            previous = ?current_endpoint,
            configured = %format!("{mqtt_host}:{mqtt_port}"),
            "Gateway address changed - recreating MQTT client"
        );
        // Dropping the client tears down its poll task via the internal
        // EventLoopGuard; the `guard` slot is re-filled below.
        *guard = None;
    }

    // Create callback that decodes MQTT protobuf messages and emits
    // structured flat-JSON events to the React frontend.
    // Also emits raw "mqtt-event" for debugging.
    let app_handle = app.clone();
    // ADR-059: the bootstrap snapshot cache lives in AppState; the
    // callback is synchronous (Fn(MqttMessage)), so updates go through
    // `try_write` — a lost write is harmless because the retained
    // snapshot is re-delivered on every (re)connect and on each
    // orchestrator version bump.
    let bootstrap_state = state.bootstrap_state.clone();
    let on_message = move |msg: MqttMessage| {
        // Always emit raw event for debugging
        let raw_payload = serde_json::json!({
            "topic": msg.topic,
            "payload_base64": base64_encode(&msg.payload),
        });
        let _ = app_handle.emit("mqtt-event", raw_payload);

        // ── Protobuf topic: `acowork/agents/+/status` ──
        //
        // The Runtime publishes its lifecycle status as a `DataEnvelope<AgentStatus>`
        // protobuf (Sept 2026 — auto-sleep retired, the only on/off transition
        // is now stop / start; see `acowork-runtime::mqtt::client::publish_status`).
        // We decode the envelope and emit a flat `agent_status` event.
        if msg.topic.starts_with("acowork/agents/") && msg.topic.ends_with("/status") {
            if let Some(parsed) = parse_agent_status_envelope(&msg.topic, &msg.payload) {
                let event = serde_json::json!({
                    "type": "agent_status",
                    "instance_id": parsed.instance_id,
                    "online": parsed.online,
                    "node_id": parsed.node_id,
                });
                let _ = app_handle.emit("agent-event", event);
                return;
            }
        }

        // ── ADR-059: Gateway bootstrap snapshot ──
        //
        // The Gateway publishes its aggregated `BootstrapState` (proto,
        // NOT a DataEnvelope) as a retained QoS-1 message on
        // `acowork/global/bootstrap`. Every push — including retained
        // re-delivery after a (re)connect — replaces the cached snapshot
        // in AppState so `get_bootstrap()` returns the freshest data
        // without an HTTP roundtrip. Also re-emitted on the
        // `bootstrap-state` Tauri channel for real-time UI updates.
        if msg.topic == "acowork/global/bootstrap" {
            match BootstrapState::decode(&msg.payload[..]) {
                Ok(state_proto) => {
                    let view = BootstrapStateView::from_proto(&state_proto);
                    tracing::debug!(
                        "[MQTT] bootstrap snapshot received phase={} version={} instance_id={}",
                        view.phase,
                        view.version,
                        view.instance_id
                    );
                    if let Ok(mut cache) = bootstrap_state.try_write() {
                        *cache = Some(view.clone());
                    }
                    let _ = app_handle.emit("bootstrap-state", view);
                }
                Err(e) => {
                    tracing::warn!(
                        "[MQTT] bootstrap payload decode failed ({} bytes): {}",
                        msg.payload.len(),
                        e
                    );
                }
            }
            return; // Not a DataEnvelope; do not fall through
        }

        // ── Inventory-change signal ──
        //
        // The Gateway publishes a **non-retained** signal on
        // `acowork/desktop/inventory` whenever its aggregated
        // `installed_agents` table mutates (a Node finishes an install /
        // uninstall, a Node replays its retained inventory on reconnect,
        // HTTP DELETE /api/agents/{id}). The payload is just a
        // millisecond timestamp — no inventory data. We forward it as an
        // `inventory-changed` Tauri event and the AgentList sidebar
        // refetches `GET /api/agents`, which is the authoritative list.
        //
        // It is deliberately NOT retained: it is a change *event*, not
        // state. The catch-up for changes we missed while disconnected
        // is the refetch on each MQTT connect edge
        // (`applyConnectionTransition` in chatStore.ts).
        if msg.topic == "acowork/desktop/inventory" {
            tracing::debug!(
                "[MQTT] inventory-change signal received ({} bytes)",
                msg.payload.len()
            );
            let ts_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let _ = app_handle.emit("inventory-changed", serde_json::json!({ "ts_ms": ts_ms }));
            return;
        }

        // Try to decode as DataEnvelope protobuf
        let envelope = match DataEnvelope::decode(&msg.payload[..]) {
            Ok(e) => e,
            Err(_) => {
                // Empty payload on a `…/sessions/{sid}/messages/{event_type}`
                // topic = the Runtime cleared a retained blocking event
                // (ask_question / tool_approval_needed) by publishing a
                // zero-byte message with `retain=true`. The broker
                // re-delivers it to subscribers, but DataEnvelope decoding
                // fails on empty bytes — so the chatStore would never learn
                // the card should disappear. Translate into a synthetic
                // `event_cleared` agent-event so the UI can drop it.
                // Reference: ChunkEvent::ClearRetainedEvent in
                // core/acowork-runtime/src/agent/loop_.rs and
                // MqttChunkPublisher::clear_retained_event in
                // core/acowork-runtime/src/mqtt/client.rs.
                if msg.payload.is_empty()
                    && let Some((sid, event_type)) = parse_retained_event_topic(&msg.topic)
                {
                    let instance_id =
                        extract_instance_id_from_topic(&msg.topic).unwrap_or_default();
                    let _ = app_handle.emit(
                        "agent-event",
                        serde_json::json!({
                            "type": "event_cleared",
                            "instance_id": instance_id,
                            "session_id": sid,
                            "event_type": event_type,
                        }),
                    );
                }
                return; // Not protobuf — ignore
            }
        };

        // ADR-073: the topic path variable under `acowork/agents/` is the
        // INSTANCE identity — the canonical addressing key. Session-scoped
        // envelopes carry only the package `agent_id` in their payload, so
        // the addressing key must be derived from the topic, never from
        // `sm.agent_id` / `created.agent_id` / ... (package identity is a
        // category attribute with display value only).
        let topic_instance_id = extract_instance_id_from_topic(&msg.topic).unwrap_or_default();

        let Some(payload) = &envelope.payload else { return };

        match payload {
            // ── Session message events (streaming) ──
            data_envelope::Payload::SessionMessage(sm) => {
                if let Some(event) = &sm.event
                    && let Some(flat) = session_message_to_flat(&topic_instance_id, sm.session_id.as_str(), event)
                {
                    let _ = app_handle.emit("agent-event", flat);
                }
            }

            // ── Session lifecycle ──
            data_envelope::Payload::SessionCreated(created) => {
                let event = serde_json::json!({
                    "type": "session_created",
                    "instance_id": topic_instance_id,
                    "session_id": created.session_id,
                    "title": created.title,
                    "created_at": created.created_at,
                });
                tracing::info!(
                    instance_id = %topic_instance_id,
                    session_id = %created.session_id,
                    title = %created.title,
                    "DESKTOP: emitting session_created agent-event"
                );
                let _ = app_handle.emit("agent-event", event);
            }
            data_envelope::Payload::SessionDeleted(deleted) => {
                let event = serde_json::json!({
                    "type": "session_deleted",
                    "instance_id": topic_instance_id,
                    "session_id": deleted.session_id,
                    "deleted_at": deleted.deleted_at,
                });
                tracing::info!(
                    instance_id = %topic_instance_id,
                    session_id = %deleted.session_id,
                    "DESKTOP: emitting session_deleted agent-event"
                );
                let _ = app_handle.emit("agent-event", event);
            }

            // ── ADR-038: explicit lifecycle acks ──
            //
            // The Runtime publishes these after handling `POST
            // /sessions/{sid}/open` (Success) or after rejecting any
            // session-level command for a non-Active session (Error).
            // The Desktop uses them to flip `isSessionReady` and
            // surface a toast with a reopen affordance respectively — see
            // `chatStore.case "session_opened"` and
            // `chatStore.case "session_not_opened"`.
            //
            // SessionOpened / SessionNotOpened proto messages do not
            // carry `agent_id` (it's encoded in the topic path
            // `acowork/agents/{instance_id}/sessions/{sid}/opened` /
            // `…/not_opened`); we parse it out of the topic so the
            // flat-JSON payload stays self-describing for the Desktop.
            data_envelope::Payload::SessionOpened(opened) => {
                let instance_id = extract_instance_id_from_topic(&msg.topic)
                    .unwrap_or_default();
                let event = serde_json::json!({
                    "type": "session_opened",
                    "instance_id": instance_id,
                    "session_id": opened.session_id,
                    "status": opened.status,
                    "model": opened.model,
                    "provider": opened.provider,
                    "last_active_at": opened.last_active_at,
                });
                let _ = app_handle.emit("agent-event", event);
            }
            data_envelope::Payload::SessionNotOpened(not_opened) => {
                let instance_id = extract_instance_id_from_topic(&msg.topic)
                    .unwrap_or_default();
                let event = serde_json::json!({
                    "type": "session_not_opened",
                    "instance_id": instance_id,
                    "session_id": not_opened.session_id,
                    "attempted_command": not_opened.attempted_command,
                    "reason": not_opened.reason,
                });
                let _ = app_handle.emit("agent-event", event);
            }

            // ── Session config (ADR-043: user-configurable fields only) ──
            data_envelope::Payload::SessionConfig(config) => {
                tracing::info!(
                    target: "llm_avail_diag",
                    session_id = %config.session_id,
                    llm_availability_raw = config.llm_availability as i32,
                    "DIAG: DESKTOP received session_config with llm_availability"
                );
                let event = serde_json::json!({
                    "type": "session_config",
                    "instance_id": topic_instance_id,
                    "session_id": config.session_id,
                    "title": config.title,
                    "provider_id": config.provider_id,
                    "account_id": config.account_id,
                    "model_id": config.model_id,
                    "reasoning_effort": config.reasoning_effort,
                    "temperature": config.temperature,
                    "workspace_id": config.workspace_id,
                    // Three-state LLM availability (ADR-XXX). prost enums
                    // don't derive serde, so serialize the wire tag
                    // explicitly; the frontend maps i32 → its projection.
                    "llm_availability": config.llm_availability as i32,
                });
                tracing::info!(
                    instance_id = %topic_instance_id,
                    session_id = %config.session_id,
                    model_id = %config.model_id,
                    provider_id = %config.provider_id,
                    workspace_id = %config.workspace_id,
                    "DESKTOP: emitting session_config agent-event"
                );
                let _ = app_handle.emit("agent-event", event);
            }

            // ── Session state (ADR-043: runtime telemetry only) ──
            data_envelope::Payload::SessionState(state) => {
                let event = serde_json::json!({
                    "type": "session_state",
                    "instance_id": topic_instance_id,
                    "session_id": state.session_id,
                    "message_count": state.message_count,
                    "input_tokens": state.input_tokens,
                    "output_tokens": state.output_tokens,
                    "total_input_tokens": state.total_input_tokens,
                    "total_output_tokens": state.total_output_tokens,
                    "ratio": state.ratio,
                    "updated_at": state.updated_at,
                });
                // Parse status and context_usage inline
                let mut m = event.as_object().unwrap().clone();
                if !state.status.is_empty() {
                    match serde_json::from_str::<serde_json::Value>(&state.status) {
                        Ok(val) => { m.insert("status".into(), val); }
                        Err(_) => { m.insert("status".into(), serde_json::Value::String(state.status.clone())); }
                    }
                }
                if !state.context_usage.is_empty() {
                    match serde_json::from_str::<serde_json::Value>(&state.context_usage) {
                        Ok(val) => { m.insert("context_usage".into(), val); }
                        Err(_) => { m.insert("context_usage".into(), serde_json::Value::String(state.context_usage.clone())); }
                    }
                }
                tracing::info!(
                    instance_id = %topic_instance_id,
                    session_id = %state.session_id,
                    message_count = state.message_count,
                    "DESKTOP: emitting session_state agent-event"
                );
                let _ = app_handle.emit("agent-event", serde_json::Value::Object(m));
            }

            // ── Agent lifecycle ──
            data_envelope::Payload::AgentStatus(status) => {
                // Same shape as the plain-text branch above — schema
                // must be identical so the React `chatStore.case
                // "agent_status"` reducer can handle either path with
                // one code path. `sleeping` was retired in Sept 2026
                // alongside auto-sleep, so the wire now carries only
                // `online` + `instance_id` + `node_id`.
                let event = serde_json::json!({
                    "type": "agent_status",
                    // ADR-073: the envelope carries `instance_id` on the
                    // wire (field 4); fall back to the topic path for
                    // legacy envelopes that predate the field.
                    "instance_id": if status.instance_id.is_empty() {
                        topic_instance_id.clone()
                    } else {
                        status.instance_id.clone()
                    },
                    "online": status.online,
                    "node_id": status.node_id,
                });
                let _ = app_handle.emit("agent-event", event);
            }
            data_envelope::Payload::AgentMeta(meta) => {
                let event = serde_json::json!({
                    "type": "agent_meta",
                    "instance_id": if meta.instance_id.is_empty() {
                        topic_instance_id.clone()
                    } else {
                        meta.instance_id.clone()
                    },
                    "name": meta.name,
                    "version": meta.version,
                });
                let _ = app_handle.emit("agent-event", event);
            }
            data_envelope::Payload::AgentConfig(config) => {
                let event = serde_json::json!({
                    "type": "agent_config",
                    "instance_id": if config.instance_id.is_empty() {
                        topic_instance_id.clone()
                    } else {
                        config.instance_id.clone()
                    },
                    "config_json": config.config_json,
                });
                let _ = app_handle.emit("agent-event", event);
            }

            // ── Sidecar ──
            data_envelope::Payload::SidecarStatus(sc) => {
                let event = serde_json::json!({
                    "type": "sidecar_status",
                    "kind": sc.kind,
                    "endpoint": sc.endpoint,
                    "ready": sc.ready,
                });
                let _ = app_handle.emit("agent-event", event);
            }

            // ── Memory node update ──
            data_envelope::Payload::MemoryNodeUpdate(update) => {
                let event = serde_json::json!({
                    "type": "memory_node_update",
                    "instance_id": topic_instance_id,
                    "node_id": update.node_id,
                    "node_json": update.node_json,
                });
                let _ = app_handle.emit("agent-event", event);
            }

            // ── Debug protocol events (ADR-048 D6) ──
            //
            // Runtime DevMode events arrive on
            // `acowork/agents/glm-5.3_common/debug/events/{event_type}`.
            // The protobuf payloads carry `session_id` but NOT `agent_id`
            // (it lives in the topic path), so we re-attach it here the
            // same way the SessionOpened / SessionNotOpened arms do.
            //
            // These are emitted on a dedicated `debug-event` Tauri channel
            // (not `agent-event`): the debugStore owns their state and the
            // chatStore's `handleMessageEvent` must not need to know about
            // debug types. Payload `type` mirrors the MQTT topic suffix so
            // the frontend dispatch table stays 1:1 with the wire topics.
            data_envelope::Payload::DebugStepEvent(ev) => {
                let instance_id = extract_instance_id_from_topic(&msg.topic).unwrap_or_default();
                let event = serde_json::json!({
                    "type": "onStep",
                    "instance_id": instance_id,
                    "session_id": ev.session_id,
                    "iteration": ev.iteration,
                    "phase": ev.phase,
                    "prompt_tokens": ev.prompt_tokens,
                    "completion_tokens": ev.completion_tokens,
                    "total_tokens": ev.total_tokens,
                });
                let _ = app_handle.emit("debug-event", event);
            }
            data_envelope::Payload::DebugContextBuiltEvent(ev) => {
                let instance_id = extract_instance_id_from_topic(&msg.topic).unwrap_or_default();
                // sections: proto map<string, SectionMeta> -> flat JSON
                // object keyed by section name (system_prompt, ...). The
                // frontend ContextSnapshotMeta consumes it as a plain
                // Record and iterates the fixed SECTION_ORDER list.
                let sections: serde_json::Map<String, serde_json::Value> = ev
                    .sections
                    .iter()
                    .map(|(name, meta)| {
                        (
                            name.clone(),
                            serde_json::json!({
                                "size_bytes": meta.size_bytes,
                                "token_estimate": meta.token_estimate,
                                "hash": meta.hash,
                            }),
                        )
                    })
                    .collect();
                let event = serde_json::json!({
                    "type": "onContextBuilt",
                    "instance_id": instance_id,
                    "session_id": ev.session_id,
                    "iteration": ev.iteration,
                    "total_token_estimate": ev.total_token_estimate,
                    "sections": serde_json::Value::Object(sections),
                    // ADR-054 step 2: control params carried on the event so
                    // the metadata bar renders without a follow-up RPC.
                    "request_params": ev.request_params.as_ref().map(|rp| serde_json::json!({
                        "model": rp.model,
                        "temperature": rp.temperature,
                        "max_tokens": rp.max_tokens,
                        "reasoning_effort": rp.reasoning_effort,
                        "thinking_mode": rp.thinking_mode,
                    })),
                });
                let _ = app_handle.emit("debug-event", event);
            }
            data_envelope::Payload::DebugStateChangeEvent(ev) => {
                let instance_id = extract_instance_id_from_topic(&msg.topic).unwrap_or_default();
                // `new_state` carries either a DebugState ("Running" /
                // "Paused" / "Stepping" / "Stopped") or a DebugPhase name
                // ("LlmCall", ...) - the Runtime maps both legacy event
                // kinds onto this topic. The frontend discriminates by
                // value (see debugStore `_handleDebugEvent`).
                let event = serde_json::json!({
                    "type": "onStateChange",
                    "instance_id": instance_id,
                    "session_id": ev.session_id,
                    "new_state": ev.new_state,
                    "iteration": ev.iteration,
                });
                let _ = app_handle.emit("debug-event", event);
            }

            // ── Workspace FS change events (ADR-058) ──
            //
            // Runtime's WorkspaceFsWatcher publishes aggregated batches
            // on `acowork/agents/{id}/workspaces/{wid}/fs-changed`
            // (QoS 1, non-retained). Re-emitted on the dedicated
            // `acowork:workspace-fs-changed` Tauri channel — the
            // workspaceStore / fileEditorStore own this state; the
            // chatStore's `handleMessageEvent` must not know about it
            // (same channel-separation rationale as debug-event).
            data_envelope::Payload::WorkspaceFsChangeEvent(ev) => {
                let changes: Vec<serde_json::Value> = ev
                    .changes
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "kind": fs_change_kind_str(c.kind),
                            "path": c.path,
                            "timestamp_ms": c.timestamp_ms,
                        })
                    })
                    .collect();
                let event = serde_json::json!({
                    // ADR-073: the topic path carries the instance identity;
                    // the payload `agent_id` is package metadata (display only)
                    // and must not be used for store addressing.
                    "instance_id": topic_instance_id,
                    "workspace_id": ev.workspace_id,
                    "changes": changes,
                    "window_end_ms": ev.window_end_ms,
                });
                let _ = app_handle.emit("acowork:workspace-fs-changed", event);
            }

            // ── Git state changed (ADR-078 follow-up) ──
            //
            // Runtime publishes a bare "re-read git status" nudge on
            // `acowork/agents/{id}/workspaces/{wid}/git-changed` after a
            // shell tool call that may have mutated index/HEAD. The
            // payload deliberately carries NO git state — the frontend
            // re-reads `GET /git/status`, keeping git semantics
            // server-side (ADR-009 v2).
            //
            // Re-emitted on its own Tauri channel (same channel-
            // separation rationale as fs-changed / debug-event): the
            // chatStore's `handleMessageEvent` must not know about it.
            data_envelope::Payload::GitStatusChanged(ev) => {
                let event = serde_json::json!({
                    // ADR-073: the topic path carries the instance identity;
                    // the payload `agent_id` is package metadata (display only)
                    // and must not be used for store addressing.
                    "instance_id": topic_instance_id,
                    "workspace_id": ev.workspace_id,
                    "window_end_ms": ev.window_end_ms,
                });
                let _ = app_handle.emit("acowork:workspace-git-changed", event);
            }

            // ── Doc library tree changes ──
            //
            // acowork-doc publishes on `acowork/doc/tree/changed`
            // (QoS 1, non-retained) after every structural mutation.
            // Re-emitted on the dedicated `acowork:doc-tree-changed`
            // Tauri channel — the docTreeStore owns this state, the
            // chatStore must not know about it (same channel-separation
            // rationale as workspace-fs-changed / debug-event).
            data_envelope::Payload::DocTreeChanged(ev) => {
                let _ = app_handle.emit(
                    "acowork:doc-tree-changed",
                    serde_json::json!({
                        "changed_dirs": ev.changed_dirs,
                    }),
                );
            }

            // ── Global resources & control commands: ignore (Gateway handles these) ──
            // DebugBreakpointEvent / DebugRecordStepEvent are likewise
            // reserved-but-unemitted (see Runtime mqtt/debug_events.rs);
            // they fall through here until handlers exist.
            _ => {}
        }
    };

    let client = DesktopMqttClient::connect(
        &mqtt_host,
        mqtt_port,
        user_id,
        mqtt_credentials,
        on_message,
        // ADR-036 / ADR-039: bridge `rumqttc` eventloop status → Tauri event.
        //
        // The `mqtt-status` event is BEST-EFFORT real-time notification.
        // The source of truth lives in `DesktopMqttClient::session_state`
        // (a watch channel updated synchronously by the poll task, which
        // `get_mqtt_status` reads).  This callback must stay synchronous
        // and side-effect-free apart from `app.emit` -- any state mutation
        // here would re-introduce the race the architecture was refactored
        // to avoid.
        move |status| {
            let payload = match &status {
                MqttStatus::Connected => serde_json::json!({
                    "connected": true,
                }),
                MqttStatus::Connecting => serde_json::json!({
                    "connected": false,
                    "connecting": true,
                }),
                MqttStatus::Reconnecting { reason } => serde_json::json!({
                    "connected": false,
                    "reconnecting": true,
                    "reason": reason,
                }),
            };
            if let Err(e) = app.emit("mqtt-status", payload) {
                tracing::warn!(error = %e, "failed to emit mqtt-status");
            }
        },
    ).await?;

    // ADR-065 Step 4: subscriptions are now driven by
    // `DesktopHandler::on_connack` (re-applied on every (re)connect),
    // so no explicit subscribe call is needed here. The handler covers
    // every entry in ALL_TOPIC_FILTERS (lifecycle + session messages),
    // which guarantees the initial subscription set matches the
    // post-reconnect set — eliminating the gap that previously caused
    // silent event loss after a reconnect.

    let shared = Arc::new(tokio::sync::Mutex::new(client));
    *guard = Some(shared);
    // Record the endpoint this client was created for so a later
    // `connect_mqtt` call can detect a Gateway-address change.
    *state.mqtt_endpoint.lock().await = Some((mqtt_host, mqtt_port));

    tracing::info!("Desktop MQTT client connected and subscribed to all agent topics");
    Ok(())
}

/// Disconnect the MQTT client.
#[tauri::command]
pub async fn disconnect_mqtt(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.mqtt_client.lock().await;
    *guard = None;
    // Clear the endpoint record together with the client (same lock
    // order as `connect_mqtt`: `mqtt_client` then `mqtt_endpoint`).
    *state.mqtt_endpoint.lock().await = None;
    tracing::info!("Desktop MQTT client disconnected");
    Ok(())
}

/// Force a soft-restart of the MQTT client.
///
/// Drops the current `EventLoop` and creates a fresh `AsyncClient` +
/// `EventLoop` pair, then re-subscribes to all topics. Use this when
/// the MQTT connection appears stuck (e.g. status shows "Reconnecting"
/// for an extended period, or messages stop arriving despite the broker
/// being healthy).
///
/// Unlike `disconnect_mqtt` (which tears down the client entirely),
/// this keeps the poll task alive and automatically recovers the
/// connection.
#[tauri::command]
pub async fn force_reconnect_mqtt(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let guard = state.mqtt_client.lock().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| "MQTT client not connected".to_string())?;

    let client = client.lock().await;
    client.force_reconnect();
    tracing::info!("MQTT force-reconnect triggered by user");
    Ok(())
}

/// Snapshot of the current MQTT connection status, returned to the frontend.
///
/// ADR-036 / ADR-039: the source of truth is the `SessionState` watch
/// channel held by `DesktopMqttClient`.  The poll task updates it
/// synchronously inside its `on_status` callback (no `tokio::spawn`
/// indirection), so this read is guaranteed to reflect the latest
/// transition observed by the poll task.
///
/// The frontend's `initMqttListener` calls this AFTER `listen()`
/// resolves, so it never races the `mqtt-status` event either way:
///   - If the event was emitted before `listen()` registered, the
///     snapshot below returns the same value as the event would have.
///   - If `listen()` registered first, future events flow normally.
///
/// `known: false` means the MQTT client is not yet connected (or has
/// been torn down).  The frontend uses this to avoid flashing the
/// disconnected banner on cold start.
#[tauri::command]
pub async fn get_mqtt_status(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let guard = state.mqtt_client.lock().await;
    let Some(client) = guard.as_ref() else {
        eprintln!("[get_mqtt_status] no client exists");
        return Ok(serde_json::json!({
            "known": false,
            "connected": false,
            "reason": null,
        }));
    };
    let client = client.lock().await;
    let state = client.session_state();
    Ok(mqtt_status_to_payload(&state))
}
/// Map a proto `FsChangeKind` (encoded as i32) to the string form the
/// frontend stores dispatch on ("created" / "modified" / "deleted").
fn fs_change_kind_str(kind: i32) -> &'static str {
    match acowork_core::mqtt_proto::FsChangeKind::try_from(kind) {
        Ok(acowork_core::mqtt_proto::FsChangeKind::Created) => "created",
        Ok(acowork_core::mqtt_proto::FsChangeKind::Modified) => "modified",
        Ok(acowork_core::mqtt_proto::FsChangeKind::Deleted) => "deleted",
        _ => "unspecified",
    }
}

/// Derive the MQTT broker host from the Gateway HTTP base URL
/// (ADR-058 §3.5 / W4).
///
/// Remote mode expects the user to forward BOTH ports to the same host
/// (e.g. `ssh -L 19876:localhost:19876 -L 19875:localhost:19875 wsl`),
/// so the Gateway HTTP host is also the broker host. Returns `None` for
/// unparseable URLs — the caller falls back to the localhost default.
fn derive_mqtt_broker_host(gateway_base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(gateway_base_url).ok()?;
    url.host_str().map(|h| h.to_string())
}

/// Whether the active MQTT client (identified by the endpoint recorded
/// in `AppState::mqtt_endpoint`) already targets the configured broker.
///
/// `connect_mqtt` uses this to pick between the fast path (existing
/// client reused) and the rebuild path (Gateway address changed while a
/// stale client was still connected — see the idempotence-guard comment
/// in `connect_mqtt`).
fn endpoint_matches(recorded: Option<&(String, u16)>, host: &str, port: u16) -> bool {
    match recorded {
        Some((h, p)) => h == host && *p == port,
        None => false,
    }
}

#[cfg(test)]
mod mqtt_endpoint_tests {
    use super::*;

    /// The idempotence guard must rebuild whenever the recorded broker
    /// endpoint differs from the configured one (SplashScreen retry /
    /// Settings address change) and short-circuit only on an exact
    /// (host, port) match.
    #[test]
    fn endpoint_matches_requires_identical_host_and_port() {
        let recorded = Some(("192.168.3.61".to_string(), 19875));
        assert!(endpoint_matches(recorded.as_ref(), "192.168.3.61", 19875));
        // Different host — the 67 → 61 address-change incident.
        assert!(!endpoint_matches(recorded.as_ref(), "192.168.3.67", 19875));
        // Different port — e.g. a custom mqtt_port discovered via /api/status.
        assert!(!endpoint_matches(recorded.as_ref(), "192.168.3.61", 19876));
        // No client recorded yet (fresh start / after disconnect).
        assert!(!endpoint_matches(None, "192.168.3.61", 19875));
    }
}

#[cfg(test)]
mod adr058_tests {
    use super::*;

    /// ADR-058 review M-1: the i32 → string mapping is the Rust ↔ TS
    /// contract the frontend stores dispatch on ("created" / "modified"
    /// / "deleted"). Any change here must be mirrored in
    /// `workspaceFsEvents.ts` `FsChange.kind`.
    #[test]
    fn fs_change_kind_str_maps_all_variants() {
        use acowork_core::mqtt_proto::FsChangeKind as K;
        assert_eq!(fs_change_kind_str(K::Created as i32), "created");
        assert_eq!(fs_change_kind_str(K::Modified as i32), "modified");
        assert_eq!(fs_change_kind_str(K::Deleted as i32), "deleted");
        // Unspecified + any out-of-range value degrade safely.
        assert_eq!(fs_change_kind_str(K::Unspecified as i32), "unspecified");
        assert_eq!(fs_change_kind_str(99), "unspecified");
        assert_eq!(fs_change_kind_str(-1), "unspecified");
    }

    /// ADR-058 review M-1: broker host derivation from the Gateway base
    /// URL (Remote-mode tunnel support). Local URLs yield "127.0.0.1"
    /// — identical to the previous hardcode; WSL/remote IPs pass through.
    #[test]
    fn derive_mqtt_broker_host_covers_local_and_remote() {
        assert_eq!(
            derive_mqtt_broker_host("http://127.0.0.1:19876"),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(
            derive_mqtt_broker_host("http://localhost:19876"),
            Some("localhost".to_string())
        );
        // Remote / WSL host behind an SSH tunnel.
        assert_eq!(
            derive_mqtt_broker_host("http://192.168.31.10:19876"),
            Some("192.168.31.10".to_string())
        );
        // Path/query are ignored; IPv6 brackets are stripped by host_str.
        assert_eq!(
            derive_mqtt_broker_host("http://10.0.0.5:19876/api"),
            Some("10.0.0.5".to_string())
        );
        // Unparseable input → None (caller falls back to the default).
        assert_eq!(derive_mqtt_broker_host("not a url"), None);
        assert_eq!(derive_mqtt_broker_host(""), None);
    }
}

/// Convert a `session_message::Event` protobuf oneof to flat JSON
/// matching the old WebSocket event format that `handleMessageEvent` expects.
///
/// ADR-073: `instance_id` is the canonical addressing key (derived by the
/// caller from the MQTT topic path). The envelope's package `agent_id` is
/// deliberately NOT used here — it is a category attribute with display
/// value only and would key the frontend stores wrongly.
fn session_message_to_flat(
    instance_id: &str,
    session_id: &str,
    event: &session_message::Event,
) -> Option<serde_json::Value> {
    let base = serde_json::json!({
        "instance_id": instance_id,
        "session_id": session_id,
    });

    match event {
        session_message::Event::Chunk(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("chunk".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            m.insert("delta".into(), serde_json::Value::String(p.delta.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ToolCall(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("tool_call".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            m.insert("tool_name".into(), serde_json::Value::String(p.tool_name.clone()));
            m.insert("arguments_json".into(), serde_json::Value::String(p.arguments_json.clone()));
            m.insert("call_id".into(), serde_json::Value::String(p.call_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ToolResult(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("tool_result".into()));
            m.insert("call_id".into(), serde_json::Value::String(p.call_id.clone()));
            m.insert("result_json".into(), serde_json::Value::String(p.result_json.clone()));
            m.insert("is_error".into(), serde_json::Value::Bool(p.is_error));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::Done(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("done".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::Error(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("error".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            m.insert("content".into(), serde_json::Value::String(p.error.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::Stopped(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("stopped".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::AskQuestion(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("ask_question".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            // Runtime serializes the whole ChunkEvent::AskQuestion
            // ({request_id, question, options, title, timeout_seconds}) as a
            // single JSON string in `question_json`.  The frontend's
            // `AskQuestionEvent` type expects those fields at the top level
            // (so e.g. `event.options.map(...)` works in AskQuestionCard),
            // so we MUST flatten the parsed object into `m` rather than
            // nesting it under a single key.
            match serde_json::from_str::<serde_json::Value>(&p.question_json) {
                Ok(serde_json::Value::Object(qm)) => {
                    for (k, v) in qm {
                        m.insert(k, v);
                    }
                }
                Ok(other) => {
                    // Unexpected shape (e.g. array) — surface raw so the
                    // frontend can still inspect it under `question_json`.
                    m.insert("question_json".into(), other);
                }
                Err(_) => {
                    m.insert(
                        "question_json".into(),
                        serde_json::Value::String(p.question_json.clone()),
                    );
                }
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::TodoUpdated(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("todo_list_updated".into()));
            match serde_json::from_str::<serde_json::Value>(&p.todos_json) {
                Ok(val) => { m.insert("todos".into(), val); }
                Err(_) => { m.insert("todos_json".into(), serde_json::Value::String(p.todos_json.clone())); }
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ReasoningStarted(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("reasoning_started".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ReasoningEnded(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("reasoning_ended".into()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::CompactingStarted(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("compacting_started".into()));
            m.insert("session_id".into(), serde_json::Value::String(p.session_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::CompactingEnded(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("compacting_ended".into()));
            m.insert("session_id".into(), serde_json::Value::String(p.session_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        // ADR-083: compaction ended without a summary. `reason` is carried as
        // a stable string ("user" | "timeout" | "failed") so the frontend can
        // pick the matching toast without knowing the proto enum numbering.
        session_message::Event::CompactionCancelled(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("compaction_cancelled".into()));
            m.insert("session_id".into(), serde_json::Value::String(p.session_id.clone()));
            let reason = match acowork_core::mqtt_proto::CompactionCancelReason::try_from(p.reason) {
                Ok(acowork_core::mqtt_proto::CompactionCancelReason::User) => "user",
                Ok(acowork_core::mqtt_proto::CompactionCancelReason::Timeout) => "timeout",
                Ok(acowork_core::mqtt_proto::CompactionCancelReason::Failed) => "failed",
                _ => "failed",
            };
            m.insert("reason".into(), serde_json::Value::String(reason.into()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ContextUsage(p) => {
            // Prefer the fully-populated `context_usage` payload when the
            // Runtime publishes it: it carries `context_window`, `total_tokens`,
            // `usage_percent` and `usable_context` that the StatusBar needs.
            // Falling back to the legacy 4 token-count fields would render the
            // StatusBar with `undefined` and crash `formatTokenCount`.
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("context_usage".into()));
            // Legacy per-field tokens (kept for any older subscriber).
            m.insert("input_tokens".into(), serde_json::json!(p.input_tokens));
            m.insert("output_tokens".into(), serde_json::json!(p.output_tokens));
            m.insert("total_input_tokens".into(), serde_json::json!(p.total_input_tokens));
            m.insert("total_output_tokens".into(), serde_json::json!(p.total_output_tokens));
            if !p.context_usage.is_empty() {
                match serde_json::from_str::<serde_json::Value>(&p.context_usage) {
                    Ok(val) => { m.insert("context_usage".into(), val); }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to parse ContextUsagePayload.context_usage");
                    }
                }
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::MemoryUpdated(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("memory_updated".into()));
            m.insert("node_id".into(), serde_json::Value::String(p.node_id.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::SkillExecuted(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("skill_executed".into()));
            m.insert("skill_name".into(), serde_json::Value::String(p.skill_name.clone()));
             m.insert("success".into(), serde_json::Value::Bool(p.success));
            Some(serde_json::Value::Object(m))
        }
        // ADR-043: SessionStateChanged payload deleted from SessionMessage.
        // Runtime state (status, ratio, context_usage) now flows through
        // the retained `sessions/{sid}/state` topic (Payload::SessionState).
        session_message::Event::LoopDetectedPaused(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("loop_detected_paused".into()));
            m.insert("session_id".into(), serde_json::Value::String(p.session_id.clone()));
            m.insert("message".into(), serde_json::Value::String(p.message.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::IterationLimitPaused(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("iteration_limit_paused".into()));
            m.insert("iteration".into(), serde_json::json!(p.iteration));
            m.insert("max_iterations".into(), serde_json::json!(p.max_iterations));
            m.insert("message".into(), serde_json::Value::String(p.message.clone()));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ToolApprovalNeeded(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("tool_approval_needed".into()));
            m.insert("request_id".into(), serde_json::Value::String(p.request_id.clone()));
            m.insert("tool_name".into(), serde_json::Value::String(p.tool_name.clone()));
            m.insert("action".into(), serde_json::Value::String(p.action.clone()));
            m.insert("risk_level".into(), serde_json::Value::String(p.risk_level.clone()));
            m.insert("reason".into(), serde_json::Value::String(p.reason.clone()));
            m.insert("tool_call_id".into(), serde_json::Value::String(p.tool_call_id.clone()));
            m.insert("approval_timeout_secs".into(), serde_json::json!(p.approval_timeout_secs));
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::NewDataAvailable(p) => {
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("new_data_available".into()));
            m.insert("interval_ms".into(), serde_json::json!(p.interval_ms));
            if !p.title.is_empty() {
                m.insert("title".into(), serde_json::Value::String(p.title.clone()));
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::StreamDelta(p) => {
            // ADR-035: incremental streaming delta carrying whole new lines.
            // Each line is ALWAYS a complete line (never a partial/token), so
            // the frontend appends without re-splitting.
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("stream_delta".into()));
            let lines: Vec<serde_json::Value> = p
                .lines
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "role": l.role,
                        "message_id": l.message_id,
                        "line_no": l.line_no,
                        "content": l.content,
                    })
                })
                .collect();
            m.insert("lines".into(), serde_json::Value::Array(lines));
            // Per-session monotonic seq (ADR-035). The frontend's
            // `insertBySeq` needs it to place the streaming placeholder at
            // the correct position in messages[] under broker reorder.
            // Omitted (None) only for pre-seq Runtimes — frontend then
            // falls back to append-to-end.
            if let Some(seq) = p.seq {
                m.insert("seq".into(), serde_json::json!(seq));
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::RecordComplete(p) => {
            // ADR-035: a record finalized (committed to JSONL), carrying the
            // COMPLETE content. Frontend freezes the active stream into
            // messages[] on receipt.
            //
            // For tool_call / tool_result records the backend now forwards
            // tool_name / tool_call_id / is_error so the frontend can
            // reconstruct the pairing without an HTTP round-trip. For
            // assistant / thought these fields are empty / false; we still
            // emit them so the frontend's switch statement stays uniform.
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("record_complete".into()));
            m.insert("role".into(), serde_json::Value::String(p.role.clone()));
            m.insert("message_id".into(), serde_json::Value::String(p.message_id.clone()));
            m.insert("content".into(), serde_json::Value::String(p.content.clone()));
            m.insert("tool_name".into(), serde_json::Value::String(p.tool_name.clone()));
            m.insert("tool_call_id".into(), serde_json::Value::String(p.tool_call_id.clone()));
            m.insert("is_error".into(), serde_json::Value::Bool(p.is_error));
            // Per-session monotonic seq — MUST match the seq of the matching
            // stream_delta placeholder so the frontend freeze lands at the
            // same slot; also orders direct tool_call / tool_result records.
            if let Some(seq) = p.seq {
                m.insert("seq".into(), serde_json::json!(seq));
            }
            Some(serde_json::Value::Object(m))
        }
        session_message::Event::ToolProgress(p) => {
            // ADR-045: Tool execution progress heartbeat.
            // Frontend uses this to refresh a timer/countdown display.
            // Does NOT carry tool result data — pure control-plane signal.
            let mut m = base.as_object().unwrap().clone();
            m.insert("type".into(), serde_json::Value::String("tool_progress".into()));
            m.insert("tool_call_id".into(), serde_json::Value::String(p.tool_call_id.clone()));
            m.insert("elapsed_ms".into(), serde_json::json!(p.elapsed_ms));
            m.insert("timeout_ms".into(), serde_json::json!(p.timeout_ms));
            Some(serde_json::Value::Object(m))
        }
    }
}

/// Simple base64 encoder (no external dependency needed for tests).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        result.push(if chunk.len() > 1 { CHARS[((triple >> 6) & 0x3f) as usize] } else { b'=' } as char);
        result.push(if chunk.len() > 2 { CHARS[(triple & 0x3f) as usize] } else { b'=' } as char);
    }
    result
}

/// Parse a session-scoped blocking-event topic.
///
/// Recognised shape:
///   `acowork/agents/{instance_id}/sessions/{sid}/messages/{event_type}`
///
/// Returns `(session_id, event_type)` for topics matching this shape, or
/// `None` otherwise. The Runtime only uses this slot for retained
/// blocking events (`ask_question`, `tool_approval_needed`); a zero-byte
/// payload on this topic means "clear the previously retained event".
fn parse_retained_event_topic(topic: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() >= 7
        && parts[0] == "acowork"
        && parts[1] == "agents"
        && parts[3] == "sessions"
        && parts[5] == "messages"
    {
        let sid = parts[4];
        let event_type = parts[6];
        if !sid.is_empty() && !event_type.is_empty() {
            return Some((sid.to_string(), event_type.to_string()));
        }
    }
    None
}

/// Extract the INSTANCE identity segment from a session-scoped MQTT topic.
///
/// ADR-073: the path variable under `acowork/agents/` is the instance id
/// (UUID v4) — the canonical addressing key. All session topics share the
/// prefix `acowork/agents/{instance_id}/sessions/{sid}/...`.
/// Returns `None` for topics that don't match this shape (which should
/// only happen on malformed/misconfigured topics).
fn extract_instance_id_from_topic(topic: &str) -> Option<String> {
    let parts: Vec<&str> = topic.split('/').collect();
    // acowork / agents / {id} / sessions / ...  (>=3rd segment, 0-indexed)
    if parts.len() >= 3 && parts[0] == "acowork" && parts[1] == "agents" {
        let instance_id = parts[2];
        if !instance_id.is_empty() {
            return Some(instance_id.to_string());
        }
    }
    None
}

/// Parse the plain-text agent status payload published by the Runtime
/// Decode a `DataEnvelope<AgentStatus>` published by the Runtime on
/// `acowork/agents/{instance_id}/status` (retained message).
///
/// Returns:
/// - `Some(...)` when the topic matches the status shape AND the
///   payload decodes as a `DataEnvelope<AgentStatus>`.
/// - `None` when the topic doesn't match (caller falls through) or
///   when the envelope fails to decode. Decode failures are logged
///   once-per-shape — a corrupt envelope should not flood the log,
///   but a total absence of decoding on every retained message (the
///   2026-09-25 WARN-spam incident caused by the legacy plaintext
///   discriminator eating protobuf bytes) must be loud.
///
/// Auto-sleep was retired in Sept 2026 — there is no longer a
/// `sleeping` field. The protobuf schema keeps `instance_id` (field
/// 4) and `node_id` (field 5) so the wire shape is stable across
/// version bumps.
fn parse_agent_status_envelope(topic: &str, payload: &[u8]) -> Option<ParsedAgentStatus> {
    if !topic.starts_with("acowork/agents/") || !topic.ends_with("/status") {
        return None;
    }
    let envelope = DataEnvelope::decode(payload).ok()?;
    let status = match envelope.payload {
        Some(data_envelope::Payload::AgentStatus(s)) => s,
        _ => {
            tracing::warn!(
                topic = %topic,
                "agent status topic received a DataEnvelope without an AgentStatus payload — ignoring"
            );
            return None;
        }
    };
    // Prefer the envelope's instance_id; fall back to the topic so a
    // misconfigured Runtime that forgot to set the field still surfaces
    // a useful row in the sidebar (matches Gateway behaviour).
    let topic_instance = extract_instance_id_from_topic(topic).unwrap_or_default();
    let instance_id = if !status.instance_id.is_empty() {
        status.instance_id.clone()
    } else {
        topic_instance
    };
    Some(ParsedAgentStatus {
        instance_id,
        online: status.online,
        node_id: status.node_id,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedAgentStatus {
    instance_id: String,
    online: bool,
    node_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_instance_id_from_opened_topic() {
        assert_eq!(
            extract_instance_id_from_topic("acowork/agents/com.acowork.pm/sessions/sess-1/opened"),
            Some("com.acowork.pm".to_string())
        );
    }

    #[test]
    fn extract_instance_id_from_not_opened_topic() {
        assert_eq!(
            extract_instance_id_from_topic("acowork/agents/agent_x/sessions/sess-9/not_opened"),
            Some("agent_x".to_string())
        );
    }

    #[test]
    fn extract_instance_id_missing_returns_none() {
        assert_eq!(extract_instance_id_from_topic(""), None);
        assert_eq!(extract_instance_id_from_topic("not/the/expected/topic"), None);
        assert_eq!(extract_instance_id_from_topic("acowork/agents//sessions/x/opened"), None);
    }

    /// Auto-sleep was retired in Sept 2026 — `sleeping` is no longer a
    /// status value. The Runtime now publishes only `online=true` /
    /// `online=false` via a `DataEnvelope<AgentStatus>` protobuf.
    /// Round-trip an online envelope and verify the Desktop decoder
    /// surfaces the structured fields.
    #[test]
    fn parse_envelope_online_payload() {
        use acowork_core::mqtt_proto::data_envelope::Payload;
        use acowork_core::mqtt_proto::AgentStatus;

        let env = DataEnvelope {
            version: 1,
            payload: Some(Payload::AgentStatus(AgentStatus {
                agent_id: "com.acowork.weather".to_string(),
                online: true,
                instance_id: "uuid-online".to_string(),
                node_id: "local".to_string(),
            })),
        };
        let bytes = prost::Message::encode_to_vec(&env);
        let p = parse_agent_status_envelope(
            "acowork/agents/uuid-online/status",
            &bytes,
        )
        .expect("online envelope must parse");
        assert_eq!(p.instance_id, "uuid-online");
        assert!(p.online);
        assert_eq!(p.node_id, "local");
    }

    #[test]
    fn parse_envelope_offline_payload() {
        use acowork_core::mqtt_proto::data_envelope::Payload;
        use acowork_core::mqtt_proto::AgentStatus;

        let env = DataEnvelope {
            version: 1,
            payload: Some(Payload::AgentStatus(AgentStatus {
                agent_id: "com.acowork.weather".to_string(),
                online: false,
                instance_id: "uuid-offline".to_string(),
                node_id: "remote-node".to_string(),
            })),
        };
        let bytes = prost::Message::encode_to_vec(&env);
        let p = parse_agent_status_envelope(
            "acowork/agents/uuid-offline/status",
            &bytes,
        )
        .unwrap();
        assert_eq!(p.instance_id, "uuid-offline");
        assert!(!p.online);
        assert_eq!(p.node_id, "remote-node");
    }

    /// Regression: a non-status topic (e.g. `acowork/agents/{id}/sessions/x/...`)
    /// must NOT be parsed — the function short-circuits on the topic shape
    /// before touching the payload, regardless of whether the payload
    /// happens to decode as a valid envelope.
    #[test]
    fn parse_envelope_non_status_topic_returns_none() {
        use acowork_core::mqtt_proto::data_envelope::Payload;
        use acowork_core::mqtt_proto::AgentStatus;

        let env = DataEnvelope {
            version: 1,
            payload: Some(Payload::AgentStatus(AgentStatus {
                agent_id: "com.acowork.x".to_string(),
                online: true,
                instance_id: "x".to_string(),
                node_id: "local".to_string(),
            })),
        };
        let bytes = prost::Message::encode_to_vec(&env);
        assert!(
            parse_agent_status_envelope("acowork/agents/x/sessions/s-1/meta", &bytes).is_none(),
            "non-status topics must not be intercepted — let the protobuf decoder branch handle them"
        );
    }

    /// Regression: a status-topic payload that is NOT a valid
    /// `DataEnvelope<AgentStatus>` must return `None` without panic.
    /// (The previous plaintext parser used `from_utf8_lossy` on a
    /// binary blob and emitted a spurious WARN every retained
    /// replay — the source of the 2026-09-25 WARN-spam incident.)
    #[test]
    fn parse_envelope_garbage_returns_none() {
        assert!(
            parse_agent_status_envelope(
                "acowork/agents/x/status",
                &[0x08, 0x01, 0xff, 0xfe],
            )
            .is_none()
        );
    }

    /// Regression: a valid envelope that does NOT carry an
    /// `AgentStatus` payload (e.g. an unrelated DataEnvelope type
    /// accidentally published on the status topic) must return
    /// `None` so the caller falls through.
    #[test]
    fn parse_envelope_wrong_payload_variant_returns_none() {
        use acowork_core::mqtt_proto::data_envelope::Payload;
        use acowork_core::mqtt_proto::BootstrapState;

        let env = DataEnvelope {
            version: 1,
            payload: Some(Payload::BootstrapState(BootstrapState::default())),
        };
        let bytes = prost::Message::encode_to_vec(&env);
        assert!(
            parse_agent_status_envelope("acowork/agents/x/status", &bytes).is_none(),
            "wrong payload variant must NOT be misreported as AgentStatus"
        );
    }
}

/// Convert a `SessionState` into the JSON payload returned by `get_mqtt_status`
/// and emitted as `mqtt-status` events.  Centralised so the snapshot and
/// the event use byte-for-byte identical shapes.
fn mqtt_status_to_payload(state: &acowork_mqtt_session::SessionState) -> serde_json::Value {
    use acowork_mqtt_session::SessionState;
    match state {
        SessionState::Idle => serde_json::json!({
            "known": false,
            "connected": false,
            "reason": null,
        }),
        // `Connecting` means the client exists and is actively trying to
        // connect (initial connect or after force_reconnect).  We return
        // `known: true` so the frontend updates its store and starts the
        // polling fallback, rather than ignoring the snapshot.
        SessionState::Connecting => serde_json::json!({
            "known": true,
            "connected": false,
            "connecting": true,
            "reason": null,
        }),
        SessionState::Reconnecting => serde_json::json!({
            "known": true,
            "connected": false,
            "reconnecting": true,
            "reason": "reconnecting",
        }),
        SessionState::Connected => serde_json::json!({
            "known": true,
            "connected": true,
        }),
        SessionState::Disconnected { reason } => serde_json::json!({
            "known": true,
            "connected": false,
            "reason": reason,
        }),
    }
}
