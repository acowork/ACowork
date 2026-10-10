//! ADR-091 §5.5/§5.7: Gateway-side reverse proxy for the Node's LSP registry API.
//!
//! `/api/nodes/{node_id}/lsp-admin/{*tail}` → Node `http://{http_endpoint}/lsp/{tail}`.
//!
//! Deliberately a second route next to the ADR-089 relay passthrough
//! (`/api/nodes/{id}/lsp/*`), for the two reasons the ADR records:
//!
//! 1. **Different tier.** The passthrough is read-only tooling traffic and
//!    needs `can_view`; this surface creates and deletes registry entries and
//!    runs install scripts, so every method that is not a read needs
//!    `can_manage` (D14).
//! 2. **Different readiness.** The passthrough requires the relay to be up
//!    (`lsp_endpoint`); the registry API has to work precisely when it is not
//!    — a missing or misconfigured language server is what brought the user to
//!    this screen. Only `online` + `http_endpoint` are required here (§5.5).
//!
//! Permission is decided here, not on the Node: the Node holds one credential
//! (its node token) and has no notion of a caller identity, so it cannot tell
//! a viewer from a manager (§5.7). The node token is injected on the forwarded
//! request and stays the Node's only auth boundary.
//!
//! There is no WebSocket branch — the registry surface is plain HTTP, so this
//! handler is a buffered forward in both directions.

use axum::body::Body;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use reqwest::header::HeaderValue;

use crate::gateway::ownership::{self, OwnerRecord};
use crate::http::auth_middleware::AuthContext;
use crate::http::nodes_api::node_owner_rec;
use crate::http::proxy::{is_hop_by_hop_header, node_token_for};
use crate::http::routes::{ApiError, AppState};

pub fn lsp_admin_routes() -> Router<AppState> {
    Router::new().route("/api/nodes/{id}/lsp-admin/{*tail}", any(proxy_lsp_admin))
}

/// Ownership tier a registry call needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Reveals node state (server list, install state, script text).
    View,
    /// Changes node state, or runs a script on it.
    Manage,
}

/// Reads are `GET`/`HEAD`; everything else mutates the registry or executes a
/// script (D14: custom create / `PUT` / `DELETE` / `install` are the manage
/// set).
fn required_tier(method: &Method) -> Tier {
    match *method {
        Method::GET | Method::HEAD => Tier::View,
        _ => Tier::Manage,
    }
}

/// Why a request was refused before it reached the Node.
#[derive(Debug, PartialEq, Eq)]
enum Rejection {
    /// Unknown node, or one whose visibility excludes the caller — rendered as
    /// 404 so the two are indistinguishable (no existence leak, same rule as
    /// the ADR-089 passthrough).
    Invisible,
    /// Visible but not manageable — the caller may still read the list, so
    /// this is a real 403 with an actionable message.
    Forbidden,
}

/// Decide the gate from a caller context and the node's ownership row.
///
/// `auth == None` is Local mode / machine actor: ADR-087 D8 gives the single
/// local user full manage rights, so nothing here is refused.
///
/// Split out from [`gate`] so the tier matrix is assertable without an
/// `AppState` and a node registry.
fn decide(
    tier: Tier,
    auth: Option<&AuthContext>,
    rec: Option<&OwnerRecord>,
) -> Result<(), Rejection> {
    let Some(ctx) = auth else {
        return Ok(());
    };
    if !ownership::can_view(rec, &ctx.user_id, ctx.is_admin()) {
        return Err(Rejection::Invisible);
    }
    if tier == Tier::Manage && !ownership::can_manage(rec, &ctx.user_id, ctx.is_admin()) {
        return Err(Rejection::Forbidden);
    }
    Ok(())
}

fn rejection_response(rejection: Rejection, node_id: &str) -> Response {
    match rejection {
        Rejection::Invisible => ApiError::not_found("node not found").into_response(),
        Rejection::Forbidden => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": format!("managing LSP servers on node {node_id} requires owner or admin rights"),
                "code": "forbidden",
            })),
        )
            .into_response(),
    }
}

/// Node-state failures on the way to the registry (ADR-089 D4 vocabulary,
/// minus `relay_not_ready`: registry traffic does not need a live relay).
#[derive(Debug)]
enum Unavailable {
    Offline,
    NoEndpoint,
    NotEnrolled,
}

fn unavailable_response(failure: Unavailable) -> Response {
    let (message, code) = match failure {
        Unavailable::Offline => ("node is offline", "node_offline"),
        Unavailable::NoEndpoint => (
            "node has not reported an HTTP endpoint",
            "node_endpoint_missing",
        ),
        Unavailable::NotEnrolled => (
            "node is not enrolled; cannot authenticate to its proxy",
            "node_not_enrolled",
        ),
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": message, "code": code })),
    )
        .into_response()
}

#[allow(clippy::too_many_arguments)] // axum handler: extractor list, not an API
pub async fn proxy_lsp_admin(
    State(state): State<AppState>,
    Path((node_id, tail)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    auth: Option<Extension<AuthContext>>,
    body: axum::body::Bytes,
) -> Response {
    let tier = required_tier(&method);

    // Ownership row + node record: one registry read each, both needed before
    // any forwarding decision.
    let rec = node_owner_rec(&state, &node_id);
    if let Err(rejection) = decide(tier, auth.as_ref().map(|e| &e.0), rec.as_ref()) {
        tracing::warn!(node_id = %node_id, method = %method, ?tier, "Refused LSP registry admin call");
        return rejection_response(rejection, &node_id);
    }

    let node = {
        let Some(registry) = state.node_registry.as_ref() else {
            return unavailable_response(Unavailable::Offline);
        };
        registry.read().await.get(&node_id).cloned()
    };
    let Some(node) = node else {
        // Same 404 shape as an invisible node — the caller learns nothing about
        // whether this id exists.
        return rejection_response(Rejection::Invisible, &node_id);
    };
    if !node.online {
        return unavailable_response(Unavailable::Offline);
    }
    let base = node
        .info
        .as_ref()
        .map(|i| i.http_endpoint.clone())
        .filter(|ep| !ep.is_empty())
        .unwrap_or_default();
    if base.is_empty() {
        return unavailable_response(Unavailable::NoEndpoint);
    }
    let node_token = match node_token_for(&state, &node_id).await {
        Some(token) => token,
        None => return unavailable_response(Unavailable::NotEnrolled),
    };

    forward_to_node(&base, &node_id, &node_token, tail, method, headers, query, body).await
}

/// Buffered forward of one registry call to the Node's `/lsp/...` surface.
/// The parameter list is the forwarded request itself (target, credential,
/// tail, method, headers, query, body) — grouping it would only rename it.
#[allow(clippy::too_many_arguments)]
async fn forward_to_node(
    base: &str,
    node_id: &str,
    node_token: &str,
    tail: String,
    method: Method,
    headers: HeaderMap,
    query: Option<String>,
    body: axum::body::Bytes,
) -> Response {
    let path = format!("/lsp/{tail}");
    let target_url = match &query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    };

    let client = crate::http::lsp_proxy::relay_proxy_client();
    let mut request = client.request(method, &target_url);
    for (name, value) in &headers {
        if !is_hop_by_hop_header(name) {
            request = request.header(name, value);
        }
    }
    let request = request.header("X-ACowork-Node-Token", node_token);
    // Create / update payloads are JSON bodies the Node's handlers parse; a
    // dropped body would surface as a 400 far from its real cause.
    let request = if body.is_empty() {
        request
    } else {
        request.body(body.to_vec())
    };

    match request.send().await {
        Ok(response) => {
            let status = StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let resp_headers = response.headers().clone();
            let resp_body = response.bytes().await.unwrap_or_default();
            let mut builder = Response::builder().status(status);
            *builder.headers_mut().unwrap() = resp_headers;
            builder.body(Body::from(resp_body)).unwrap_or_else(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to build LSP registry proxy response",
                )
                    .into_response()
            })
        }
        Err(e) => {
            tracing::warn!(node_id = %node_id, url = %target_url, error = %e, "Failed to proxy to node LSP registry");
            (
                StatusCode::BAD_GATEWAY,
                [(
                    HeaderName::from_static("x-error-origin"),
                    HeaderValue::from_static("gateway"),
                )],
                Json(serde_json::json!({
                    "error": "failed to connect to the node LSP registry API",
                    "code": "node_unreachable",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::account::Role;
    use ownership::OwnerRecord;

    fn ctx(user_id: &str, role: Role) -> AuthContext {
        AuthContext {
            user_id: user_id.to_string(),
            role,
            as_user: None,
        }
    }

    /// A private node owned by `alice`.
    fn private_node() -> OwnerRecord {
        OwnerRecord::new(Some("alice".to_string()))
    }

    /// A node whose metadata is public to every logged-in user (ADR-087 D6:
    /// that widens *view* only).
    fn public_node() -> OwnerRecord {
        OwnerRecord {
            visibility: ownership::Visibility::Public,
            ..OwnerRecord::new(Some("alice".to_string()))
        }
    }

    #[test]
    fn only_reads_are_view_tier() {
        assert_eq!(required_tier(&Method::GET), Tier::View);
        assert_eq!(required_tier(&Method::HEAD), Tier::View);
        // Install is a POST and must not slip through as a read.
        for write in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            assert_eq!(required_tier(&write), Tier::Manage, "{write}");
        }
    }

    /// §5.7: the gate must be the ADR-087 matrix, not a coarse "logged in".
    #[test]
    fn gate_matrix_for_a_private_node() {
        let rec = private_node();

        // Owner: reads and writes.
        assert!(decide(Tier::View, Some(&ctx("alice", Role::User)), Some(&rec)).is_ok());
        assert!(decide(Tier::Manage, Some(&ctx("alice", Role::User)), Some(&rec)).is_ok());

        // Stranger: neither — a private node's registry is not readable.
        assert_eq!(
            decide(Tier::View, Some(&ctx("bob", Role::User)), Some(&rec)),
            Err(Rejection::Invisible)
        );
        assert_eq!(
            decide(Tier::Manage, Some(&ctx("bob", Role::User)), Some(&rec)),
            Err(Rejection::Invisible)
        );

        // Admin sees everything (ADR-087 D7: that is how an unclaimed node is
        // found and claimed).
        assert!(decide(Tier::Manage, Some(&ctx("carol", Role::Admin)), Some(&rec)).is_ok());

        // Local mode / machine actor: no ownership layer (ADR-087 D8).
        assert!(decide(Tier::Manage, None, None).is_ok());
    }

    /// A guest may use a node's tooling but must not reshape it — the
    /// distinction the two tiers exist for (ADR-087 D9).
    #[test]
    fn a_public_or_guest_node_is_readable_but_not_writable() {
        let rec = public_node();
        let stranger = ctx("bob", Role::User);
        assert!(decide(Tier::View, Some(&stranger), Some(&rec)).is_ok());
        assert_eq!(
            decide(Tier::Manage, Some(&stranger), Some(&rec)),
            Err(Rejection::Forbidden)
        );

        let guest_rec = OwnerRecord {
            guests: vec!["bob".to_string()],
            ..OwnerRecord::new(Some("alice".to_string()))
        };
        assert!(decide(Tier::View, Some(&stranger), Some(&guest_rec)).is_ok());
        assert_eq!(
            decide(Tier::Manage, Some(&stranger), Some(&guest_rec)),
            Err(Rejection::Forbidden)
        );
    }

    /// No ownership row (a node enrolled before ADR-087) is ownerless, which
    /// D7 makes admin-only at every tier — fail-closed, not readable.
    #[test]
    fn an_ownerless_node_is_admin_only() {
        assert_eq!(
            decide(Tier::View, Some(&ctx("bob", Role::User)), None),
            Err(Rejection::Invisible)
        );
        assert_eq!(
            decide(Tier::Manage, Some(&ctx("bob", Role::User)), None),
            Err(Rejection::Invisible)
        );
        assert!(decide(Tier::Manage, Some(&ctx("admin", Role::Admin)), None).is_ok());
    }

    /// The path is the Node's own contract (`/lsp/...`, no `/api` prefix —
    /// §5.5), so the tail must be forwarded verbatim, query and body
    /// included.
    #[tokio::test]
    async fn forwards_the_tail_and_query_to_the_node_route() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let node = tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/lsp/servers/{name}/install",
                axum::routing::post(
                    |axum::extract::Path(name): axum::extract::Path<String>,
                     headers: HeaderMap,
                     RawQuery(q): RawQuery,
                     body: axum::body::Bytes| async move {
                        format!(
                            "{}|{}|{}|{}",
                            name,
                            headers
                                .get("X-ACowork-Node-Token")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("<missing>"),
                            q.unwrap_or_default(),
                            String::from_utf8_lossy(&body)
                        )
                    },
                ),
            );
            axum::serve(listener, app).await.unwrap();
        });

        let resp = forward_to_node(
            &format!("http://127.0.0.1:{port}"),
            "node-1",
            "tok",
            "servers/rust/install".to_string(),
            Method::POST,
            HeaderMap::new(),
            Some("dry=true".to_string()),
            axum::body::Bytes::from_static(br#"{"yes":true}"#),
        )
        .await;

        let status = resp.status();
        let text = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&text).to_string();
        assert_eq!(status, StatusCode::OK, "body: {text}");
        assert_eq!(text, r#"rust|tok|dry=true|{"yes":true}"#);
        node.abort();
    }

    /// A node that is not listening is a 502 with the gateway as origin — the
    /// UI must be able to tell "your node is gone" from "the registry says no".
    #[tokio::test]
    async fn an_unreachable_node_is_a_bad_gateway() {
        // Port 1: nothing accepts there.
        let resp = forward_to_node(
            "http://127.0.0.1:1",
            "node-1",
            "tok",
            "servers".to_string(),
            Method::GET,
            HeaderMap::new(),
            None,
            axum::body::Bytes::new(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            resp.headers()
                .get("x-error-origin")
                .and_then(|v| v.to_str().ok()),
            Some("gateway")
        );
    }
}
