//! First-boot restricted-mode middleware (ADR-076 §决策 12 v2).
//!
//! Until a fresh install's passwordless `admin` account (seeded by
//! `acowork-user`, `password_hash = DISABLED_PASSWORD_HASH`) gets a real
//! password, the Gateway only answers `/health` and
//! `/api/status` — every other `/api/*` request returns 403
//! `{"error":"setup_required"}`. There is no HTTP path to set the first
//! password (ADR-076 §决策 12 v2); the operator must complete setup via the
//! daemon's interactive TTY prompt, the `admin-setup` subcommand, or by
//! editing `gateway.toml`.
//!
//! Placement: inside `CORS` and *outside* `auth_middleware` — axum executes
//! layers bottom-to-top, so this one is `.layer()`-ed after the auth gate
//! (`http/routes.rs::build_router`). We want the 403 to still carry CORS
//! headers (so the Desktop WebView can read the body) and to short-circuit
//! *before* the token check: an unauthenticated restricted-mode request must
//! be a 403 `setup_required`, never the 401 `missing bearer token` the auth
//! gate would answer first.
//!
//! The middleware is a **no-op** in `AUTH_MODE=local` (no account system →
//! nothing to restrict) and a no-op once the admin has a real password.
//!
//! ADR-084: the flag no longer comes from a per-request read of
//! `accounts.json` — that store belongs to the `acowork-user` process. It
//! comes from the snapshot the user supervisor refreshes off the service's
//! `/health`, so this is O(1) and I/O-free on every path.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

use crate::http::routes::AppState;

/// Paths exempt from the restricted-mode block. These are the only
/// responses the Gateway serves until the operator finishes setup.
const PUBLIC_PREFIXES: &[&str] = &["/health", "/api/status"];

/// Middleware function — installs with `from_fn_with_state` (needs the
/// `AppState` to read `is_restricted()`).
///
/// Returns 403 with `{"error":"setup_required"}` if the Gateway is in
/// restricted mode and the request is not whitelisted. Otherwise passes
/// through.
pub async fn restricted_mode_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    // O(1) early-out: no account system = nothing to restrict.
    if !state.auth_mode.is_multi_user() {
        return next.run(req).await;
    }
    // O(1) early-out, and genuinely O(1): the flag comes from the snapshot
    // the user supervisor refreshes on its `/health` poll, so this no longer
    // re-reads `accounts.json` per request (ADR-084 §决策 4b).
    //
    // `None` = the service has not reported yet. Treated as "not restricted"
    // deliberately: during that window `auth_middleware` already answers 503
    // for every non-public path, so there is nothing to additionally gate —
    // and claiming `setup_required` here would send the Desktop into the
    // first-boot wizard on a machine that is merely still starting.
    let requires_setup = state
        .gateway_state
        .read()
        .await
        .user_snapshot
        .as_ref()
        .is_some_and(|s| s.requires_setup);
    if !requires_setup {
        return next.run(req).await;
    }
    // Whitelist: /health and /api/status remain reachable so the Desktop
    // can probe liveness and discover the `requires_setup: true` flag.
    let path = req.uri().path();
    if PUBLIC_PREFIXES.iter().any(|p| path == *p || path.starts_with(&format!("{p}/"))) {
        return next.run(req).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "setup_required",
            "message": "Gateway is in first-boot restricted mode. \
                        Complete admin setup on the Gateway host \
                        (TTY prompt / 'admin-setup' subcommand / \
                        bootstrap_admin in the user service config) \
                        before connecting."
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use crate::lifecycle::user_supervisor::UserSnapshot;
    use crate::gateway::state::GatewayState;
    use crate::http::routes::AppState;
    use crate::http::auth::HttpAuth;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn build_state(dir: &std::path::Path) -> AppState {
        let gw_state = GatewayState::new(&dir.to_string_lossy());
        AppState::new(
            Arc::new(RwLock::new(gw_state)),
            Arc::new(HttpAuth::new(false)),
        )
    }

    async fn ok_handler() -> &'static str {
        "ok"
    }

    /// Build the router state for a first-boot Gateway.
    ///
    /// Pre-ADR-084 this seeded a passwordless account and asserted
    /// `AuthService::is_restricted()`; the flag now arrives as the user
    /// supervisor's `/health` snapshot, which is what we set directly.
    fn app_with_restricted(restricted: bool) -> (Router, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = build_state(dir.path());
        if restricted {
            state.auth_mode = AuthMode::MultiUser;
            state
                .gateway_state
                .try_write()
                .expect("fresh state")
                .user_snapshot = Some(UserSnapshot {
                requires_setup: true,
                registration_open: false,
            });
        }
        let router = Router::new()
            .route("/api/anything", get(ok_handler))
            .route("/health", get(ok_handler))
            .route("/api/status", get(ok_handler))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                restricted_mode_middleware,
            ))
            .with_state(state);
        // Touch to keep COUNTER referenced (avoids unused-import lint
        // if we later drop the unique-name helper).
        let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
        (router, dir)
    }

    #[tokio::test]
    async fn restricted_mode_blocks_unlisted_routes() {
        let (router, _dir) = app_with_restricted(true);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "setup_required");
    }

    #[tokio::test]
    async fn restricted_mode_allows_health() {
        let (router, _dir) = app_with_restricted(true);
        let resp = router
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn restricted_mode_allows_status() {
        let (router, _dir) = app_with_restricted(true);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_restricted_mode_passes_through() {
        let (router, _dir) = app_with_restricted(false);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// The gate must be the *outer* middleware. Layer order only exists in
    /// the real router, so this asserts against `build_router`: with no
    /// `Authorization` header at all, a restricted Gateway must answer
    /// 403 `setup_required`, not the 401 `missing bearer token` that
    /// `auth_middleware` would produce if it ran first.
    #[tokio::test]
    async fn real_router_answers_403_not_401_without_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = build_state(dir.path());
        state.auth_mode = AuthMode::MultiUser;
        {
            let mut gw = state.gateway_state.try_write().expect("fresh state");
            gw.config = Some(crate::config::GatewayConfig {
                data_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            });
            // First-boot snapshot: a passwordless admin, as the user
            // service reports it in `/health`.
            gw.user_snapshot = Some(UserSnapshot {
                requires_setup: true,
                registration_open: false,
            });
        }
        let router = crate::http::routes::build_router(state);

        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "setup_required");

        // `/api/status` stays reachable *and* advertises the gate, so the
        // Desktop can discover the state instead of seeing a dead Gateway.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["requires_setup"], true);
    }
}