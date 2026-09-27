//! acowork-user health-check endpoint (supervisor liveness contract).
//!
//! Reuses [`acowork_core::health::HealthResponse`]: the Gateway supervisor
//! probes `GET /health` to decide whether this process is ready / alive
//! (same contract as PM / doc / embed / LSP relay).
//!
//! The `details` payload is also the Gateway's snapshot source for
//! `requires_setup` / `registration_open` (ADR-084 §决策 4b): it rides the
//! poll the supervisor already performs, so the restricted-mode gate and
//! `/api/status` need no extra round trip.

use axum::Json;
use axum::extract::State;
use serde_json::json;

use crate::state::AppState;

/// Build the `/health` route.
pub fn health_route() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/health",
        axum::routing::get(move |State(state): State<AppState>| {
            let state = state.clone();
            async move {
                let (data_dir, auth, auth_mode, registration_open) = {
                    let shared = state.shared.read().await;
                    (
                        shared.data_dir.clone(),
                        state.auth_service.clone(),
                        state.auth_mode.as_str(),
                        shared.registration_open,
                    )
                };
                // Restricted mode = no way in yet: an admin exists but every
                // admin is still passwordless (ADR-076 §决策 12 v2). Only
                // meaningful when the account system is on — under `local`
                // there is no account store to be restricted about.
                let requires_setup = auth.as_ref().is_some_and(|a| a.is_restricted());
                Json(acowork_core::health::HealthResponse {
                    status: "ok".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    process: "acowork-user".to_string(),
                    details: Some(json!({
                        "data_dir": data_dir.display().to_string(),
                        "auth_mode": auth_mode,
                        "requires_setup": requires_setup,
                        "registration_open": registration_open,
                    })),
                })
            }
        }),
    )
}
