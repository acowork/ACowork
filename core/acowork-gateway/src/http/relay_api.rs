//! Relay management API (design doc 24 §6.3): runtime enable / disable /
//! status for the Gateway's relay tunnel.
//!
//! `enable` persists the change to the config file so the tunnel
//! auto-starts on the next boot; `disable` mirrors it. The runtime
//! state itself lives in [`crate::relay::RelayClient`].

use axum::{Json, Router};
use axum::extract::State;
use axum::routing::{get, post};
use serde::Deserialize;

use crate::http::routes::{ApiError, AppState};

pub fn relay_routes() -> Router<AppState> {
    Router::new()
        .route("/api/relay/status", get(relay_status))
        .route("/api/relay/enable", post(relay_enable))
        .route("/api/relay/disable", post(relay_disable))
}

/// `GET /api/relay/status` — tunnel state snapshot.
async fn relay_status(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let client = state
        .relay_client
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("relay client not initialised"))?;
    Ok(Json(serde_json::to_value(client.status()).unwrap()))
}

#[derive(Debug, Deserialize)]
struct EnableRequest {
    /// Relay service endpoint (`wss://relay.example.com/tunnel`).
    url: String,
}

/// `POST /api/relay/enable` — start the tunnel and persist the intent.
async fn relay_enable(
    State(state): State<AppState>,
    Json(body): Json<EnableRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let client = state
        .relay_client
        .clone()
        .ok_or_else(|| ApiError::service_unavailable("relay client not initialised"))?;

    client
        .enable(body.url.clone())
        .await
        .map_err(|e| ApiError::bad_request(&format!("cannot enable relay tunnel: {e}")))?;

    // Persist so the tunnel auto-starts on the next boot. Persistence
    // failure does not undo the runtime enable — the operator sees the
    // warning and can retry (or fix disk issues) without losing the
    // tunnel that is already up.
    let mut persist_warning = None;
    {
        let mut gw = state.gateway_state.write().await;
        if let Some(config) = gw.config.as_mut() {
            config.relay.enabled = true;
            config.relay.url = Some(body.url);
            if let Err(e) = config.save() {
                tracing::warn!(error = %e, "relay: failed to persist [relay] config");
                persist_warning = Some("tunnel enabled but config persistence failed".to_string());
            }
        } else {
            persist_warning = Some("tunnel enabled but no config snapshot to persist".to_string());
        }
    }

    let mut payload = serde_json::to_value(client.status()).unwrap();
    if let Some(warning) = persist_warning {
        payload["warning"] = serde_json::Value::String(warning);
    }
    Ok(Json(payload))
}

/// `POST /api/relay/disable` — stop the tunnel and persist the intent.
async fn relay_disable(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let client = state
        .relay_client
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("relay client not initialised"))?;

    client.disable().await;

    let mut persist_warning = None;
    {
        let mut gw = state.gateway_state.write().await;
        if let Some(config) = gw.config.as_mut() {
            config.relay.enabled = false;
            if let Err(e) = config.save() {
                tracing::warn!(error = %e, "relay: failed to persist [relay] config");
                persist_warning = Some("tunnel disabled but config persistence failed".to_string());
            }
        }
    }

    let mut payload = serde_json::to_value(client.status()).unwrap();
    if let Some(warning) = persist_warning {
        payload["warning"] = serde_json::Value::String(warning);
    }
    Ok(Json(payload))
}
