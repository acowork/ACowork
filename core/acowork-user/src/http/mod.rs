//! acowork-user HTTP surface (ADR-084 §决策 6 / §决策 1).
//!
//! **Internal paths are byte-identical to the old Gateway paths** and the
//! Gateway's `user_proxy` does **not** strip the prefix (unlike `/api/doc`),
//! so the ported handlers needed no path changes and the Desktop sees the
//! same URLs it always did.
//!
//! Mode split (mirrors the Gateway's registration, ADR-076 §决策 12):
//!
//! | Mode | Registered |
//! |------|-----------|
//! | `multi_user` | `account_routes` (`/api/users` with credential semantics), `chat_routes`, `auth_routes` |
//! | `local` | `users_routes` (`/api/users` presentation-only CRUD) |
//! | both | `user_avatar_routes` |
//!
//! Never both account and presentation `/api/users` routes: axum panics on a
//! double registration of one path, and the two have different authorization
//! semantics.

pub mod account_api;
pub mod auth_api;
pub mod chat_api;
pub mod profile_api;

use axum::Router;

use crate::state::AppState;

/// Largest body this surface accepts.
///
/// The chat attachment upload endpoint raises the router-wide ceiling to
/// this value (ADR-076 §决策 9); the value is carried over from the
/// Gateway's `GLOBAL_BODY_LIMIT` so an attachment that was accepted there is
/// still accepted here.
pub const GLOBAL_BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Build the user-domain router **without** `/health` (merged by the server
/// together with the identity layer).
pub fn user_router(state: &AppState) -> Router<AppState> {
    let mut router = if state.is_multi_user() {
        account_api::account_routes().merge(chat_api::chat_routes())
    } else {
        profile_api::users_routes()
    };

    router = router.merge(profile_api::user_avatar_routes());

    if state.is_multi_user() {
        router = router.merge(auth_api::auth_routes());
    }

    router
}

/// The complete user-service surface: user domain + `/health`, behind the
/// identity layer.
///
/// One assembler so the server, the tests and (from M5) the Gateway's proxy
/// target cannot drift apart on route ordering or layer placement.
pub fn build_router(state: &AppState) -> Router<AppState> {
    Router::new()
        .merge(user_router(state))
        .merge(crate::health::health_route())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::context::inject_identity,
        ))
}
