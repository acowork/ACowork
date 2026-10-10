//! install-state convergence — the relay probes, the Node writes (ADR-091 §5.8).
//!
//! The split is deliberate: only the relay can resolve a language-server
//! binary (it owns the PATH, the profile-PATH cache and the fork+exec probe),
//! and only the Node may write `{node_home}/lsp_servers.json` (D11). So the
//! Node pulls `GET /api/lsp/status?force=true` over loopback and maps the
//! answer onto `install_state` — there is no push protocol and no shared
//! library between the two processes.
//!
//! Three triggers, all async (a forced full probe is seconds, not
//! milliseconds — §5.8):
//!
//! | trigger | where |
//! |---------|-------|
//! | the relay just became ready | [`crate::sidecar::lsp_relay_supervisor`] on the ready transition |
//! | an install just finished | `POST /lsp/servers/{name}/install` |
//! | a read found the state stale | `GET /lsp/servers` past [`crate::lsp_registry::CONVERGE_TTL`] |
//!
//! [`spawn_converge`] collapses overlapping triggers through the registry's
//! single-flight flag, so a UI that polls cannot stack probes.

use indexmap::IndexMap;

use crate::lsp_registry::{ProbeResult, SharedLspRegistry};
use crate::sidecar::lsp_relay::http_client;

/// `GET /api/lsp/status?force=true` — every configured language, probed
/// through the relay.
///
/// Returns the results keyed by the relay's canonical language name, which is
/// also the registry key (the runtime file is the relay's server list, so the
/// two key sets are the same by construction).
pub async fn fetch_probe_results(
    bind_host: &str,
    port: u16,
    force: bool,
) -> Result<IndexMap<String, ProbeResult>, String> {
    let url = format!(
        "http://{bind_host}:{port}/api/lsp/status{}",
        if force { "?force=true" } else { "" }
    );
    let body: Vec<serde_json::Value> = http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("{url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{url}: invalid JSON body: {e}"))?;

    let mut results = IndexMap::new();
    for entry in body {
        let Some(language) = entry.get("language").and_then(|v| v.as_str()) else {
            tracing::warn!(entry = %entry, "Ignoring LSP status entry without a language key");
            continue;
        };
        results.insert(
            language.to_string(),
            ProbeResult {
                installed: entry
                    .get("installed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                command: entry
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            },
        );
    }
    Ok(results)
}

/// One convergence round: probe, then write under the registry lock (§5.8).
///
/// `force=false` reuses the relay's own TTL cache, which is what the periodic
/// (stale-read) trigger wants — the relay would otherwise fork a `--version`
/// per language on every UI mount.
pub async fn converge_now(
    registry: &SharedLspRegistry,
    bind_host: &str,
    port: u16,
    force: bool,
) -> Result<usize, String> {
    if !registry.begin_convergence() {
        tracing::debug!("LSP convergence already in flight — skipping this trigger");
        return Ok(0);
    }
    let result = match fetch_probe_results(bind_host, port, force).await {
        Ok(results) => {
            let count = results.len();
            registry
                .converge(results)
                .await
                .map_err(|e| format!("failed to persist converged LSP state: {e}"))?;
            tracing::info!(
                languages = count,
                force,
                "Converged LSP install state from the relay probe"
            );
            Ok(count)
        }
        Err(e) => {
            tracing::warn!(error = %e, "LSP convergence probe failed — keeping persisted install state");
            Err(e)
        }
    };
    registry.end_convergence();
    result
}

/// Fire-and-forget convergence (every §5.8 trigger uses this).
pub fn spawn_converge(
    registry: SharedLspRegistry,
    bind_host: String,
    port: u16,
    force: bool,
    reason: &'static str,
) {
    tokio::spawn(async move {
        let _ = converge_now(&registry, &bind_host, port, force).await;
        tracing::debug!(reason, "LSP convergence trigger completed");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::lsp::LspInstallState;
    use axum::{Json, Router, routing::get};
    use crate::lsp_registry::{LspRegistry, test_entry};

    /// Stand in for the relay: serve a fixed status array and count hits.
    fn stub_relay(statuses: serde_json::Value, hits: std::sync::Arc<std::sync::atomic::AtomicU32>) -> Router {
        Router::new().route(
            "/api/lsp/status",
            get(move || {
                let body = statuses.clone();
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Json(body)
                }
            }),
        )
    }

    async fn serve(statuses: serde_json::Value) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU32>) {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits_for_server = hits.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, stub_relay(statuses, hits_for_server)).await;
        });
        (port, hits)
    }

    fn status(language: &str, installed: bool) -> serde_json::Value {
        serde_json::json!([
            { "language": language, "installed": installed, "command": installed.then_some("x-lsp") }
        ])
    }

    #[tokio::test]
    async fn converge_maps_the_probe_into_the_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = LspRegistry::load(tmp.path(), None).await;
        registry.add_custom("my-lsp", test_entry(&["my-lsp-bin"])).await.unwrap();

        let (port, hits) = serve(status("my-lsp", true)).await;
        let n = converge_now(&registry, "127.0.0.1", port, false).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);

        let entry = registry.get("my-lsp").await.unwrap();
        // Never installed by us, found on PATH → `detected`, with the command
        // the relay actually resolved.
        assert_eq!(entry.install.install_state, LspInstallState::Detected);
        assert_eq!(entry.install.installed_command.as_deref(), Some("x-lsp"));
        assert!(entry.install.last_check_ms.is_some());
    }

    /// A relay that is down (wrong port) must not be able to wipe persisted
    /// state: convergence fails, the file keeps the last known state.
    #[tokio::test]
    async fn a_failed_probe_leaves_the_registry_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = LspRegistry::load(tmp.path(), None).await;
        registry
            .add_custom("my-lsp", test_entry(&["my-lsp-bin"]))
            .await
            .unwrap();
        registry
            .finish_install("my-lsp", LspInstallState::Installed, "done", "")
            .await
            .unwrap();

        let err = converge_now(&registry, "127.0.0.1", 1, true)
            .await
            .expect_err("port 1 is unreachable");
        assert!(err.contains("api/lsp/status"), "unexpected error: {err}");
        assert_eq!(
            registry.get("my-lsp").await.unwrap().install.install_state,
            LspInstallState::Installed
        );
        // The single-flight flag was released on the failure path.
        assert!(registry.begin_convergence());
        registry.end_convergence();
    }

    /// Overlapping triggers collapse into one probe (§5.8 single-flight).
    #[tokio::test]
    async fn overlapping_triggers_probe_once() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = LspRegistry::load(tmp.path(), None).await;
        let (port, hits) = serve(serde_json::json!([])).await;

        // Claim the slot by hand, then try to converge: it must be refused
        // without touching the relay.
        assert!(registry.begin_convergence());
        let n = converge_now(&registry, "127.0.0.1", port, true).await.unwrap();
        assert_eq!(n, 0);
        assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 0);
        registry.end_convergence();

        assert!(converge_now(&registry, "127.0.0.1", port, true).await.is_ok());
        assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
