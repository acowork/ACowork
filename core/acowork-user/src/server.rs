//! acowork-user HTTP server entry.
//!
//! Runs as a **standalone process** supervised by the Gateway (ADR-084
//! §决策 5, the PM/doc pattern). [`UserService::serve`] binds a port and
//! serves the full router (user domain + `/health`) behind the identity
//! layer.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;

use crate::config::UserServiceConfig;
use crate::error::ApiError;
use crate::state::AppState;

/// The user service running instance.
pub struct UserService {
    pub config: UserServiceConfig,
    pub state: AppState,
}

impl UserService {
    /// Construct the service (no server started yet).
    pub fn new(config: UserServiceConfig, auth: Option<Arc<crate::auth::AuthService>>) -> Self {
        Self {
            state: AppState::new(&config, auth),
            config,
        }
    }

    /// Build the full router (user domain + `/health`, behind the identity
    /// layer).
    pub fn router(&self) -> Router {
        crate::http::build_router(&self.state).with_state(self.state.clone())
    }

    /// Serve the full router on `bind`, returning the **actual** bound
    /// address (reported via `--port-file` to the Gateway supervisor).
    ///
    /// Port conflict auto-increments (default 18083 up, max +20), matching
    /// PM/doc. The server runs in a background task.
    ///
    /// # Bind address
    ///
    /// Only loopback is accepted. The service trusts the `X-Auth-*` headers
    /// the Gateway injects (ADR-084 §决策 7); on any other interface a peer
    /// could forge them and impersonate any user. There is no override —
    /// adding one would mean writing a threat model for it first.
    pub async fn serve(self: Arc<Self>, bind: SocketAddr) -> Result<SocketAddr, ApiError> {
        if !bind.ip().is_loopback() {
            return Err(ApiError::bad_request(&format!(
                "acowork-user must bind loopback (got {}): it trusts Gateway-injected \
                 X-Auth-* headers, which any other host could forge (ADR-084 §决策 7)",
                bind.ip()
            )));
        }

        let host = bind.ip();
        let mut port = bind.port();
        let max_port = port.saturating_add(20);
        let router = self.router();

        loop {
            let addr = SocketAddr::new(host, port);
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    let actual = listener.local_addr().map_err(|e| {
                        ApiError::internal(&format!("failed to read bound address: {e}"))
                    })?;
                    tracing::info!(addr = %actual, "acowork-user server listening (full router)");
                    tokio::spawn(async move {
                        if let Err(e) = axum::serve(listener, router).await {
                            tracing::error!(error = %e, "acowork-user server exited with error");
                        }
                    });
                    return Ok(actual);
                }
                Err(_) if port < max_port => {
                    tracing::warn!(port, "acowork-user port occupied — trying next");
                    port += 1;
                }
                Err(e) => {
                    return Err(ApiError::internal(&format!(
                        "failed to bind acowork-user on {addr}: {e}"
                    )));
                }
            }
        }
    }
}

