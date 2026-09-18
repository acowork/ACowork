//! Authentication middleware (ADR-076 §决策 3).
//!
//! Under `AUTH_MODE=multi_user`, every `/api/*` request must carry
//! `Authorization: Bearer <access_token>`; the verified identity is
//! injected as [`AuthContext`] for downstream handlers. Under
//! `AUTH_MODE=local` no `AuthService` exists and this middleware is a
//! pass-through — the legacy bearer token keeps doing whatever it did
//! (ADR-076 §决策 12: local is a no-op for the account system).
//!
//! It is layered *inside* CORS (so a 401 still carries
//! `Access-Control-Allow-Origin` and the browser can read the error) and
//! *outside* every route (so a new route cannot forget to opt in).

use axum::{
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};

use acowork_core::account::Role;
use serde_json::json;

use crate::auth::token::now_unix;
use crate::http::routes::AppState;

/// The authenticated identity of a request (ADR-076 §决策 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub user_id: String,
    pub role: Role,
    /// ADR-076 §决策 4: admin-only "view as user X" (read-only).
    pub as_user: Option<String>,
}

impl AuthContext {
    /// Whose data this request may touch.
    ///
    /// A plain user is always themselves — `as_user` is ignored unless it
    /// was granted as an admin, which is enforced at construction.
    pub fn effective_user_id(&self) -> &str {
        match (&self.as_user, self.is_admin()) {
            (Some(u), true) => u,
            _ => &self.user_id,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// The Gateway-owned session-scope header consumed by the Runtime
/// (ADR-076 §决策 4).
///
/// **The Gateway is its only trusted writer.** The reverse proxy forwards
/// every inbound header verbatim (`proxy_to_runtime_with_method`), so a
/// client-supplied `x-user-id` would otherwise reach the Runtime first and
/// claim someone else's sessions. The middleware therefore *removes* it on
/// every request and re-inserts the token-derived value only after the
/// caller has been authenticated.
pub const USER_SCOPE_HEADER: &str = "x-user-id";

/// Scope value meaning "no filtering" — the administrator view.
pub const SCOPE_ALL: &str = "*";

/// Derive the Runtime-side session scope from an authenticated identity.
///
/// A plain user is always themselves; `as_user` applies only to an
/// administrator (already enforced at `AuthContext` construction).
fn scope_value(ctx: &AuthContext) -> String {
    match (ctx.as_user.as_deref(), ctx.is_admin()) {
        (Some(u), true) => u.to_string(),
        (_, true) => SCOPE_ALL.to_string(),
        _ => ctx.user_id.clone(),
    }
}

/// Whether an `as_user` value is safe to put in an HTTP header.
///
/// Header values reject control characters, and a value that fails to
/// build must never degrade into "no filtering" — so an out-of-shape
/// `as_user` is rejected at the boundary instead of being coerced.
fn is_valid_scope_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// Paths reachable without a login token (ADR-076 §决策 3).
///
/// Kept deliberately tiny: what a client needs *before* it can log in —
/// liveness/readiness, the bootstrap probe, the login and refresh calls,
/// the first-login call (a freshly-created account has no password yet),
/// and logout (a client with an expired access token must still be able to
/// drop a dead session). The legacy Gateway token file is never exposed.
fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/api/status"
            | "/api/bootstrap"
            | "/api/auth/login"
            | "/api/auth/refresh"
            | "/api/auth/logout"
            | "/api/auth/first-login"
    )
}

/// Bearer-token gate for `AUTH_MODE=multi_user` (ADR-076 §决策 3).
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Local mode: no account system, nothing to enforce.
    let Some(auth) = state.auth_service.clone() else {
        // The account system is off, so no identity may be asserted. Drop
        // any client-supplied scope header rather than letting it reach
        // the Runtime unvetted (ADR-076 §决策 4).
        req.headers_mut().remove(USER_SCOPE_HEADER);
        return next.run(req).await;
    };

    // Never forward a client-asserted identity: the header is re-derived
    // from the verified token below, and only for authenticated callers.
    req.headers_mut().remove(USER_SCOPE_HEADER);

    // CORS preflight must never be gated — the browser sends no
    // `Authorization` on `OPTIONS`.
    if req.method() == Method::OPTIONS || is_public_path(req.uri().path()) {
        return next.run(req).await;
    }

    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let Some(token) = token else {
        return unauthorized("missing bearer token");
    };

    let principal = match auth.verify_access(token, now_unix()) {
        Ok(p) => p,
        Err(crate::auth::AuthError::Token(e)) => {
            return unauthorized(&e.to_string());
        }
        Err(e) => return unauthorized(&e.to_string()),
    };

    // ADR-076 §决策 4: `as_user` is an admin-only read view. A non-admin
    // presenting it anywhere is a client bug or an attack — reject rather
    // than silently ignore, so it can never look like it worked.
    //
    // The read-only half is enforced here, not downstream: writes are
    // authorized against the injected scope, and `as_user` *becomes* that
    // scope — so without this guard an admin could DELETE a session while
    // the Runtime recorded the owner as the impersonated user. Admins can
    // reach the same session anyway; the point is that the audit trail must
    // not name someone who never made the request (§9 开放问题 5).
    let as_user = query_param(req.uri().query(), "as_user");
    if let Some(u) = &as_user {
        if !matches!(req.method(), &Method::GET | &Method::HEAD) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "forbidden",
                    "message": "as_user is a read-only view and cannot be used on a write request",
                })),
            )
                .into_response();
        }
        if !principal.is_admin() || !is_valid_scope_id(u) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "forbidden",
                    "message": "as_user requires an administrator token and a well-formed user id",
                })),
            )
                .into_response();
        }
    }

    let ctx = AuthContext {
        user_id: principal.user_id,
        role: principal.role,
        as_user,
    };

    // ADR-076 §决策 4: hand the Runtime the scope the Gateway derived
    // from the token — the only value it will ever see, because the same
    // header was removed from the inbound request above.
    let scope = scope_value(&ctx);
    let Ok(scope_header) = HeaderValue::from_str(&scope) else {
        // Unreachable for a validated id; fail closed regardless.
        return unauthorized("invalid session scope");
    };
    req.headers_mut().insert(USER_SCOPE_HEADER, scope_header);

    req.extensions_mut().insert(ctx);
    next.run(req).await
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized", "message": message })),
    )
        .into_response()
}

/// Percent-decoded-enough lookup of one query parameter.
///
/// Only used for `as_user`, which is a UUID: no percent-encoding is ever
/// expected, and a malformed value simply fails to match an account later.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(user: &str, role: Role, as_user: Option<&str>) -> AuthContext {
        AuthContext {
            user_id: user.into(),
            role,
            as_user: as_user.map(str::to_string),
        }
    }

    #[test]
    fn plain_user_is_always_self() {
        let c = ctx("u-1", Role::User, None);
        assert_eq!(c.effective_user_id(), "u-1");
        // as_user is rejected before construction for non-admins; even if
        // one slipped through, it must not take effect.
        let c = ctx("u-1", Role::User, Some("u-2"));
        assert_eq!(c.effective_user_id(), "u-1");
    }

    #[test]
    fn admin_as_user_switches_the_effective_identity() {
        let c = ctx("admin-1", Role::Admin, Some("u-2"));
        assert_eq!(c.effective_user_id(), "u-2");
        assert!(c.is_admin());
    }

    #[test]
    fn public_paths_are_the_pre_login_surface() {
        for p in [
            "/health",
            "/api/status",
            "/api/bootstrap",
            "/api/auth/login",
            "/api/auth/refresh",
            "/api/auth/logout",
            "/api/auth/first-login",
        ] {
            assert!(is_public_path(p), "{p} should be public");
        }
        for p in [
            "/api/auth/me",
            "/api/auth/change-password",
            "/api/agents",
            "/api/users",
            "/api/agents/foo/sessions",
        ] {
            assert!(!is_public_path(p), "{p} must require auth");
        }
    }

    #[test]
    fn scope_value_maps_identity_to_runtime_scope() {
        // Plain user → themselves, even if as_user somehow survives.
        assert_eq!(scope_value(&ctx("u-1", Role::User, Some("u-2"))), "u-1");
        // Admin without as_user → the explicit "no filtering" sentinel.
        assert_eq!(scope_value(&ctx("a-1", Role::Admin, None)), SCOPE_ALL);
        // Admin with as_user → that user.
        assert_eq!(scope_value(&ctx("a-1", Role::Admin, Some("u-2"))), "u-2");
    }

    #[test]
    fn scope_ids_are_validated_before_becoming_headers() {
        assert!(is_valid_scope_id("4f8a1c2e-0000-4000-8000-000000000001"));
        assert!(is_valid_scope_id("u_alice-1"));
        // Rejected: empty, oversized, control chars, header-injection
        // shaped, non-ascii.
        assert!(!is_valid_scope_id(""));
        assert!(!is_valid_scope_id(&"a".repeat(65)));
        assert!(!is_valid_scope_id("u-1\r\nX-Admin: 1"));
        assert!(!is_valid_scope_id("u 1"));
        assert!(!is_valid_scope_id("用户一"));
    }

    /// The whole point of ADR-076 §决策 4's transport: a client-asserted
    /// `x-user-id` must never survive to the Runtime, and an authenticated
    /// caller must get exactly one, derived from the token.
    #[tokio::test]
    async fn middleware_strips_client_scope_and_injects_the_token_scope() {
        use axum::body::Body;
        use axum::http::Request;
        use axum::{Router, routing::get};
        use tower::ServiceExt;

        let dir = std::env::temp_dir().join(format!("acowork-mw-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let svc = Arc::new(
            crate::auth::AuthService::new(
                &dir,
                Default::default(),
                Some(crate::auth::BootstrapAdmin {
                    username: "root".into(),
                    password: "Rootpass1".into(),
                    display_name: None,
                }),
            )
            .unwrap(),
        );
        svc.ensure_bootstrap_admin().unwrap();
        let admin_token = svc
            .login("root", "Rootpass1", crate::auth::token::now_unix())
            .unwrap()
            .access_token;

        let state = |multi: bool| {
            let mut st = crate::http::routes::AppState::new(
                Arc::new(tokio::sync::RwLock::new(
                    crate::gateway::state::GatewayState::new(&dir.to_string_lossy()),
                )),
                Arc::new(crate::http::auth::HttpAuth::new(false)),
            );
            if multi {
                st.auth_mode = crate::auth::AuthMode::MultiUser;
                st.auth_service = Some(svc.clone());
            }
            st
        };

        // Terminal handler: echo the scope header the Runtime would see.
        async fn echo_scope(req: Request<Body>) -> String {
            req.headers()
                .get(USER_SCOPE_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>")
                .to_string()
        }

        let app = |multi: bool| {
            Router::new()
                .route("/api/agents", get(echo_scope))
                // Write endpoint, for the read-only guard on `as_user`.
                .route("/api/agents/{sid}", axum::routing::delete(echo_scope))
                .layer(axum::middleware::from_fn_with_state(state(multi), auth_middleware))
        };
        let call = |uri: &str, token: Option<&str>| {
            let mut b = Request::builder().uri(uri).header(USER_SCOPE_HEADER, "u-victim");
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            b.body(Body::empty()).unwrap()
        };
        let body_of = |resp: Response| async move {
            axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()
        };

        // Local mode: forged header is dropped, nothing injected.
        let resp = app(false).oneshot(call("/api/agents", None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(&body_of(resp).await[..], b"<none>");

        // multi_user, no token: 401 (nothing downstream).
        let resp = app(true).oneshot(call("/api/agents", None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // multi_user, valid admin token: the forged "u-victim" is gone and
        // the token-derived scope ("*" = no filtering) is what the Runtime
        // sees.
        let resp = app(true)
            .oneshot(call("/api/agents", Some(&admin_token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(&body_of(resp).await[..], SCOPE_ALL.as_bytes());

        // as_user narrows an admin to one user's scope...
        let resp = app(true)
            .oneshot(call("/api/agents?as_user=u-alice", Some(&admin_token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(&body_of(resp).await[..], b"u-alice");

        // ...but a scope value that could not be a header fails closed.
        let resp = app(true)
            .oneshot(call("/api/agents?as_user=bad%20value", Some(&admin_token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // §9 开放问题 5: `as_user` is read-only. On a write it must be
        // rejected outright — forwarding it would make the Runtime
        // authorize (and later attribute) the write to the impersonated
        // user, so the audit trail would name someone who never asked.
        let write = |uri: &str, token: Option<&str>| {
            let mut b = Request::builder()
                .method("DELETE")
                .uri(uri)
                .header(USER_SCOPE_HEADER, "u-victim");
            if let Some(t) = token {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            b.body(Body::empty()).unwrap()
        };
        let resp = app(true)
            .oneshot(write("/api/agents/sess-1?as_user=u-alice", Some(&admin_token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // The same write without `as_user` still works as the admin.
        let resp = app(true)
            .oneshot(write("/api/agents/sess-1", Some(&admin_token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(&body_of(resp).await[..], SCOPE_ALL.as_bytes());
    }
}
