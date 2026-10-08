//! LSP Relay process lifecycle management (ADR-055 §6.7 node-local
//! sidecar).
//!
//! Migrated from the Gateway's `lifecycle/lsp_relay.rs` (the pre-cut
//! host) with a mechanical error-type re-base: `GatewayError` →
//! [`crate::error::NodeError`], `crate::lifecycle::process::*` →
//! [`crate::process::spawn`]. Semantics are unchanged — the Node is now
//! the host because the LSP server must share the filesystem with the
//! agent workspace (`root_uri = file://{workspace_root}`).
//!
//! The relay's self-exit watchdog (CLI arg `--gateway-health-url`, name
//! kept for historical compatibility) is retargeted from the Gateway
//! `/health` to the Node's own `/health` endpoint — the parent liveness
//! signal is now the Node, not the Gateway.

use std::fs;
use std::path::Path;
use std::process::Stdio;

use crate::error::{NodeError, Result};

/// Shared HTTP client for LSP Relay REST calls.
///
/// Single static `reqwest::Client` reused across the sidecar module to
/// take advantage of connection pooling — mirrors the pattern the
/// Gateway used (`http/proxy.rs::runtime_http_client`).
pub(crate) fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("Failed to build LSP Relay HTTP client")
    })
}

/// Default port for the LSP Relay service.
pub const LSP_RELAY_DEFAULT_PORT: u16 = 19878;

/// The address the relay binds, given the node's config (ADR-089 D1).
///
/// Default is loopback: the relay is unauthenticated, its only
/// consumer (the Desktop) reaches it through the two-hop Gateway →
/// Node reverse proxy, and the retained `lsps` topic value is a health
/// signal, not a routable address. `expose_lsp_relay = true` is the
/// operator escape hatch that re-exposes the relay on
/// `advertise_host` (LAN) — the historical ADR-055 §6.3 D3 behavior.
///
/// The supervisor's health/SSE probes MUST use this same host: a probe
/// aimed at loopback against a LAN-bound relay (or vice versa) never
/// turns ready, which is the ADR-089 §2.2 disappearing-node regression.
pub fn relay_bind_host(expose_lsp_relay: bool, advertise_host: &str) -> String {
    if expose_lsp_relay {
        advertise_host.to_string()
    } else {
        "127.0.0.1".to_string()
    }
}

/// Build the relay's argv. Split out from [`spawn_lsp_relay`] so the
/// bind-host contract is assertable without spawning the real binary.
///
/// ADR-089 D1: `--host` comes from [`relay_bind_host`] — loopback by
/// default, `advertise_host` only behind the `expose_lsp_relay`
/// escape hatch.
fn relay_argv(bind_host: &str, port: u16, node_health_url: &str) -> Vec<String> {
    vec![
        "--host".into(),
        bind_host.into(),
        "--port".into(),
        port.to_string(),
        "--gateway-health-url".into(),
        node_health_url.into(),
        "--log-level".into(),
        "info".into(),
    ]
}

/// State of the LSP Relay process.
#[derive(Debug, Clone)]
pub struct LspRelayProcessState {
    /// PID of the running acowork-lsp-relay process.
    /// Zero means an already-running external LSP Relay.
    pub pid: u32,
    /// Port the LSP Relay is listening on.
    pub port: u16,
    /// Whether the process has completed startup and health check.
    pub ready: bool,
}

/// Health check result from acowork-lsp-relay.
#[derive(Debug, Clone)]
pub struct LspRelayHealthStatus {
    pub ready: bool,
    /// Number of configured languages (from /health details).
    pub language_count: Option<usize>,
}

/// Create state for an already-running LSP Relay.
///
/// This is used when the Node detects that an LSP Relay is already
/// listening on the expected port (e.g. started manually for debugging).
pub fn attach_existing_lsp_relay(
    port: u16,
    health: Option<LspRelayHealthStatus>,
) -> LspRelayProcessState {
    LspRelayProcessState {
        pid: 0,
        port,
        ready: health.as_ref().map(|h| h.ready).unwrap_or(false),
    }
}

/// Spawn the acowork-lsp-relay process.
///
/// The LSP Relay runs as a sibling process to the Node, listening on
/// `127.0.0.1:{port}`. It provides WebSocket LSP relay and JSON-RPC
/// API for codebase tools.
///
/// # Arguments
///
/// * `data_dir` — Node data directory (for the log file).
/// * `port` — Port for the LSP Relay to listen on.
/// * `node_health_url` — Node health URL for self-exit detection
///   (passed through to the relay CLI arg `--gateway-health-url`, whose
///   name is kept for compatibility; semantically it is now the
///   *parent process* health URL, and the parent is the Node).
///
/// Returns `(LspRelayProcessState, Child)` — the caller is responsible
/// for reaping the child process (e.g. spawning a task that awaits
/// `child.wait()` and clears state on exit).
///
/// Note: The LSP Relay discovers its config directory via the
/// `ACOWORK_LSP_CONFIG_DIR` environment variable (inherited from the
/// Node process) or the sibling-directory fallback chain in
/// `acowork-lsp-relay/src/config.rs`. No explicit `--lsp-config-dir`
/// argument is passed.
///
/// `bind_host` is the address the relay listens on — see
/// [`relay_bind_host`] (ADR-089 D1: loopback by default; the Desktop
/// reaches the relay through the Gateway → Node reverse proxy, so the
/// advertised `lsps` topic value is a health signal, not a routable
/// address). The supervisor's health/SSE probes use the same host.
pub async fn spawn_lsp_relay(
    data_dir: &Path,
    port: u16,
    node_health_url: &str,
    bind_host: &str,
) -> Result<(LspRelayProcessState, tokio::process::Child)> {
    // Locate the acowork-lsp-relay binary (sibling of current executable)
    let relay_bin = std::env::current_exe()
        .map_err(|e| NodeError::Lifecycle(format!("Cannot find current executable: {}", e)))?
        .parent()
        .map(|p| {
            let bin_name = if cfg!(windows) {
                "acowork-lsp-relay.exe"
            } else {
                "acowork-lsp-relay"
            };
            p.join(bin_name)
        })
        .unwrap_or_else(|| {
            let bin_name = if cfg!(windows) {
                "acowork-lsp-relay.exe"
            } else {
                "acowork-lsp-relay"
            };
            std::path::PathBuf::from(bin_name)
        });

    if !relay_bin.exists() {
        return Err(NodeError::Lifecycle(format!(
            "acowork-lsp-relay binary not found at {:?}",
            relay_bin
        )));
    }

    // Create log directory and open log file (truncate on each start)
    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&log_dir).map_err(|e| {
        NodeError::Lifecycle(format!("Failed to create log dir {:?}: {}", log_dir, e))
    })?;
    let log_path = log_dir.join("lsp-relay.log");
    let log_file = fs::File::create(&log_path).map_err(|e| {
        NodeError::Lifecycle(format!(
            "Failed to create LSP Relay log file {:?}: {}",
            log_path, e
        ))
    })?;
    tracing::info!(path = %log_path.display(), "LSP Relay process logging to file");

    let mut cmd = tokio::process::Command::new(&relay_bin);
    cmd.args(relay_argv(bind_host, port, node_health_url))
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file));

    // On Unix, create a new process group
    #[cfg(unix)]
    #[allow(unused_imports)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd.spawn().map_err(|e| {
        NodeError::Lifecycle(format!(
            "Failed to spawn acowork-lsp-relay (binary: {:?}): {}",
            relay_bin, e
        ))
    })?;

    let pid = child.id().ok_or_else(|| {
        NodeError::Lifecycle(
            "Failed to get PID for acowork-lsp-relay (process may have exited immediately)"
                .to_string(),
        )
    })?;

    tracing::info!(
        "Spawned acowork-lsp-relay process (PID: {}, port: {}, host: {})",
        pid,
        port,
        bind_host
    );

    Ok((
        LspRelayProcessState {
            pid,
            port,
            ready: false,
        },
        child,
    ))
}

/// Kill the LSP Relay process.
pub async fn kill_lsp_relay(pid: u32) -> Result<()> {
    crate::process::spawn::kill_agent_process(pid).await
}

/// Check if the LSP Relay is healthy by calling its /health endpoint.
///
/// `host` is the relay's bind host ([`relay_bind_host`]) — probing a
/// different address than the one the relay listens on never succeeds
/// (ADR-089 §2.2: the disappearing-node regression).
///
/// Returns `None` if the health check fails (process not started,
/// not responding, or invalid response).
pub async fn check_lsp_relay_health(host: &str, port: u16) -> Option<LspRelayHealthStatus> {
    let url = format!("http://{host}:{}/health", port);
    let client = http_client();

    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;

    let status = body.get("status")?.as_str()?.to_string();
    let language_count = body
        .get("details")
        .and_then(|d| d.get("language_count"))
        .and_then(|c| c.as_u64())
        .map(|n| n as usize);

    Some(LspRelayHealthStatus {
        ready: status == "ok",
        language_count,
    })
}

/// Wait for the LSP Relay to become healthy.
///
/// Polls the /health endpoint at the given interval until the relay
/// reports "ok" or the timeout expires.
///
/// Returns `Some(health)` if healthy within the timeout, `None` otherwise.
pub async fn wait_for_lsp_relay_ready(
    host: &str,
    port: u16,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) -> Option<LspRelayHealthStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(health) = check_lsp_relay_health(host, port).await
            && health.ready
        {
            return Some(health);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the value following `flag` out of the argv.
    fn argv_value(argv: &[String], flag: &str) -> String {
        let i = argv.iter().position(|a| a == flag).expect("flag present");
        argv[i + 1].clone()
    }

    #[test]
    fn relay_bind_host_defaults_to_loopback() {
        // ADR-089 D1: the relay is unauthenticated and its only consumer
        // (Desktop) reaches it through the Gateway → Node reverse proxy,
        // so the bind stays on loopback even when the node advertises a
        // LAN IP.
        assert_eq!(relay_bind_host(false, "192.168.17.113"), "127.0.0.1");
        assert_eq!(relay_bind_host(true, "192.168.17.113"), "192.168.17.113");
    }

    #[test]
    fn relay_argv_carries_the_bind_host() {
        // The argv is a pure pass-through of the computed bind host —
        // the contract that broke in the ADR-089 §2.2 regression is the
        // bind/probe pairing, not the argv itself.
        let argv = relay_argv("127.0.0.1", 19878, "http://127.0.0.1:19900/health");
        assert_eq!(argv_value(&argv, "--host"), "127.0.0.1");
        assert_eq!(argv_value(&argv, "--port"), "19878");
    }

    #[test]
    fn test_lsp_relay_process_state_construction() {
        let state = LspRelayProcessState {
            pid: 12345,
            port: 19878,
            ready: true,
        };
        assert_eq!(state.pid, 12345);
        assert_eq!(state.port, 19878);
        assert!(state.ready);
    }

    #[test]
    fn test_attach_existing_lsp_relay_with_health() {
        let health = LspRelayHealthStatus {
            ready: true,
            language_count: Some(12),
        };
        let state = attach_existing_lsp_relay(19878, Some(health));
        assert_eq!(state.pid, 0);
        assert_eq!(state.port, 19878);
        assert!(state.ready);
    }

    #[test]
    fn test_attach_existing_lsp_relay_without_health() {
        let state = attach_existing_lsp_relay(19878, None);
        assert_eq!(state.pid, 0);
        assert_eq!(state.port, 19878);
        assert!(!state.ready);
    }

    #[test]
    fn test_default_port_constant() {
        // Ensure the default port is a valid, non-privileged port
        const { assert!(LSP_RELAY_DEFAULT_PORT > 1024) };
        assert_eq!(LSP_RELAY_DEFAULT_PORT, 19878);
    }

    #[tokio::test]
    async fn test_check_lsp_relay_health_unreachable() {
        // Port 1 is always unreachable — health check should return None
        let result = check_lsp_relay_health("127.0.0.1", 1).await;
        assert!(result.is_none(), "Expected None for unreachable port");
    }

    #[tokio::test]
    async fn test_wait_for_lsp_relay_ready_timeout() {
        // Port 1 is always unreachable — should timeout and return None
        let result = wait_for_lsp_relay_ready(
            "127.0.0.1",
            1,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(result.is_none(), "Expected None on timeout");
    }

    #[tokio::test]
    async fn test_wait_for_lsp_relay_ready_succeeds_when_healthy() {
        // Start a minimal HTTP server that returns a valid health response.
        // We use a raw TCP listener with manual HTTP response construction.
        let json_body = r#"{"status":"ok","process":"test","version":"0.1","details":{"language_count":5}}"#;
        let content_len = json_body.len();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        content_len, json_body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });

        // Give the mock server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let result = wait_for_lsp_relay_ready(
            "127.0.0.1",
            port,
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(100),
        )
        .await;

        assert!(result.is_some(), "Expected health status");
        let health = result.unwrap();
        assert!(health.ready);
        assert_eq!(health.language_count, Some(5));
    }
}
