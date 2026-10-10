//! ADR-090: MCP first-enable ensure (install action lives in the Agent process).
//!
//! When the user opens an MCP server's toggle in the Desktop Tools panel,
//! the Runtime (not the Gateway) makes the server usable **on this machine**:
//!
//! ```text
//! health_check(spawn)  ──ok──▶  Usable (binary already on PATH — no install)
//!        │
//!     not-found
//!        ▼
//! run_install(spec) ──ok──▶ health_check again ──ok──▶ Usable
//!        │                                    │
//!      error                                error
//!        ▼                                    ▼
//!     Failed ─────────────────────────▶ Failed (server auto-disabled,
//!                                        toggle springs back to off)
//! ```
//!
//! "Installed or not" is never persisted — the local PATH is the shared
//! state (ADR-090 D6). The in-process registry below only tracks *in-flight
//! and last-outcome* status so the UI can render 探测中/本机可用/失败 and the
//! toggle can spring back on failure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use acowork_core::protocol::{McpServerConfigDef, McpTransportDef};

use crate::agent_config;
use crate::mcp_notify::McpNotifyRef;

/// One entry in the ensure registry.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EnsureStatus {
    /// "probing" | "installing" | "usable" | "failed"
    pub state: String,
    /// Detailed failure reason (missing runtime + hint, install stderr
    /// summary, or handshake error). Only set when `state == "failed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// In-process ensure registry: server name → last status.
///
/// ponytail: process-wide map assumes one agent per Runtime process (true
/// today — the Runtime is spawned per agent instance). Ceiling: if a future
/// Runtime hosts multiple agents with same-named servers, key by
/// (instance_id, name).
fn registry() -> &'static Mutex<HashMap<String, EnsureStatus>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, EnsureStatus>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn set_status(name: &str, status: EnsureStatus) {
    registry()
        .lock()
        .expect("ensure registry lock poisoned")
        .insert(name.to_string(), status);
}

/// Snapshot of the current ensure status for `name` (None = never ensured).
pub fn ensure_status(name: &str) -> Option<EnsureStatus> {
    registry().lock().expect("ensure registry lock poisoned").get(name).cloned()
}

/// Classify a `health_check` error: is the spawn command missing
/// (⇒ install can help), or is it a different failure (⇒ install is
/// pointless and the raw reason must surface)?
fn is_command_missing(err: &str) -> bool {
    let e = err.to_lowercase();
    // Known spawn-failure shapes (see acowork-mcp client tests — the exact
    // text differs across platforms and transports):
    e.contains("program not found")
        || e.contains("entity not found")
        || e.contains("kind: notfound")
        || e.contains("no such file or directory")
        || e.contains("not recognized")
        // Windows: the stdio wrapper exits 1 on spawn failure and the
        // handshake dies with this message instead of an io::Error.
        || e.contains("mcp server closed stdout")
}

/// Build the spawn def for a health check: env with the user bin dirs
/// (pipx home, `~/.cargo/bin`, ...) prepended to PATH, so a binary
/// installed by any agent on this machine resolves (ADR-090 "PATH is the
/// shared state").
fn spawn_def_with_path(def: &McpServerConfigDef) -> McpServerConfigDef {
    let mut d = def.clone();
    d.env = acowork_mcp::build_spawn_env(&def.env);
    d
}

/// The core ensure-usable routine (ADR-090 §5.4, D5/D6).
///
/// Returns `Ok(())` when the server is usable on this machine. On any
/// failure returns a detailed, user-actionable reason.
async fn ensure_usable_inner(def: &McpServerConfigDef) -> Result<(), String> {
    let spawn = spawn_def_with_path(def);
    // 1. Probe first — PATH is the state. If the binary is already here
    //    (installed by this or any other agent on this machine), done.
    match acowork_mcp::health_check(&spawn).await {
        Ok(_tools) => return Ok(()),
        Err(e) if !is_command_missing(&e) => {
            // Connectable failure that an install cannot fix.
            return Err(e);
        }
        Err(_) => {} // command missing → proceed to install
    }

    // 2. Install (declarative spec only — no shell). npm/uvx kinds derive
    //    no install command (lazy `npx -y` / `uvx` resolve on first spawn);
    //    `run_install` returns Ok immediately for them, and the re-probe
    //    below is what actually downloads + starts the server. The runtime
    //    probe runs regardless — a missing `npx`/`uvx` is exactly the
    //    command-not-found we just saw, and the hint is user-actionable.
    let Some(spec) = def.install.as_ref() else {
        return Err(
            "MCP server command not found on this machine and no install spec is attached \
             (custom server — install the command manually or add it via a preset)"
                .to_string(),
        );
    };
    set_status(&def.name, EnsureStatus { state: "installing".into(), error: None });

    if let Err(missing) = acowork_mcp::ensure_runtime(&spec.package) {
        // Dependency (pipx/cargo/docker/node...) absent — never auto-install
        // underlying runtimes (ADR-072 user-agency principle).
        let acowork_mcp::DependencyStatus::Missing { runtime, install_hint } = missing else {
            unreachable!("ensure_runtime only returns Missing on Err");
        };
        return Err(format!(
            "Missing runtime '{runtime}'. {install_hint} — install it first, then retry."
        ));
    }
    let output = acowork_mcp::run_install(&spec.package)
        .await
        .map_err(|e| format!("install failed: {e}"))?;
    if !output.success {
        let stderr: String = output.stderr.chars().take(2000).collect();
        return Err(format!(
            "install command exited with code {:?}: {stderr}",
            output.exit_code
        ));
    }

    // 3. Re-probe.
    match acowork_mcp::health_check(&spawn).await {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("installed but health check still fails: {e}")),
    }
}

/// Remove a server from `active_names` (toggle spring-back, ADR-090 D6).
/// Read-modify-write; no-op when the name is not active.
fn deactivate_server(work_dir: &Path, name: &str) {
    let Some(mut cfg) = agent_config::load_agent_mcp_config(work_dir)
        .ok()
        .flatten()
    else {
        return;
    };
    let Some(active) = cfg.active_names.as_ref() else {
        return;
    };
    if !active.iter().any(|n| n == name) {
        return;
    }
    cfg.active_names = Some(active.iter().filter(|n| n.as_str() != name).cloned().collect());
    if let Err(e) = agent_config::save_agent_mcp_config(work_dir, &cfg) {
        tracing::error!(error = %e, server = %name, "ADR-090: failed to persist auto-deactivate after ensure failure");
    } else {
        tracing::info!(
            server = %name,
            "ADR-090: ensure failed — server deactivated (toggle springs back)"
        );
    }
}

/// Spawn the ensure task for one server if it needs ensuring.
///
/// A server "needs ensure" when: transport is stdio, it carries an install
/// spec (preset-added), and no ensure is currently running. Custom stdio
/// servers (no spec) keep the pre-ADR-090 behavior — the connection pool
/// simply fails and reports, nothing is auto-installed.
///
/// Idempotent while an ensure is in flight (second open-toggle click sees
/// the running "probing"/"installing" status, no duplicate task — the
/// package-manager race D7 accepted only applies to *different processes*).
pub fn spawn_ensure(def: McpServerConfigDef, work_dir: PathBuf, notifier: McpNotifyRef) {
    if def.transport != McpTransportDef::Stdio || def.install.is_none() {
        return;
    }
    let already_running = registry()
        .lock()
        .expect("ensure registry lock poisoned")
        .get(&def.name)
        .map(|s| s.state == "probing" || s.state == "installing")
        .unwrap_or(false);
    if already_running {
        return;
    }
    set_status(&def.name, EnsureStatus { state: "probing".into(), error: None });
    let name = def.name.clone();
    tokio::spawn(async move {
        match ensure_usable_inner(&def).await {
            Ok(()) => {
                set_status(&name, EnsureStatus { state: "usable".into(), error: None });
                // Reconnect the pool so the now-usable server's tools land
                // in agent_mcp_tools.json without a user round-trip.
                if let Some(n) = notifier {
                    n.notify();
                }
            }
            Err(error) => {
                tracing::warn!(server = %name, %error, "ADR-090 ensure_usable failed");
                set_status(&name, EnsureStatus { state: "failed".into(), error: Some(error) });
                deactivate_server(&work_dir, &name);
                if let Some(n) = notifier {
                    n.notify();
                }
            }
        }
    });
}

/// Trigger ensure for every newly-enabled server in `enabled` (the diff of
/// a `PUT /agents/{id}/mcp-servers` against the previous selection).
pub fn ensure_newly_enabled(
    work_dir: &Path,
    enabled: &[String],
    notifier: McpNotifyRef,
) {
    let Some(cfg) = agent_config::load_agent_mcp_config(work_dir).ok().flatten() else {
        return;
    };
    for name in enabled {
        if let Some(def) = cfg.catalog.iter().find(|d| &d.name == name) {
            spawn_ensure(def.clone(), work_dir.to_path_buf(), notifier.clone());
        }
    }
}

/// Clear the registry entry when the user closes the toggle (retry = reopen,
/// D6 — a stale "failed" must not shadow the next attempt's status).
pub fn clear_status(name: &str) {
    if let Ok(mut reg) = registry().lock() {
        reg.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_missing_classification() {
        assert!(is_command_missing("failed to create transport: program not found"));
        assert!(is_command_missing("...Os { code: 2, kind: NotFound, message: \"系统找不到指定的文件。\" }"));
        assert!(!is_command_missing("timed out waiting for initialize response"));
        assert!(!is_command_missing("rejected initialize: -32601"));
    }

    #[test]
    fn registry_roundtrip_and_clear() {
        let name = "adr090-test-server";
        assert!(ensure_status(name).is_none());
        set_status(name, EnsureStatus { state: "probing".into(), error: None });
        assert_eq!(ensure_status(name).unwrap().state, "probing");
        clear_status(name);
        assert!(ensure_status(name).is_none());
    }

    #[tokio::test]
    async fn ensure_fails_fast_on_non_spawn_error_without_spec() {
        // A stdio def with a bogus command and no install spec: health_check
        // fails with command-missing → must NOT be reported as connectable
        // failure, and without a spec the error mentions "no install spec".
        let def = McpServerConfigDef {
            name: "adr090-no-spec".into(),
            display_name: None,
            transport: McpTransportDef::Stdio,
            command: "definitely-not-a-real-binary-adr090".into(),
            ..Default::default()
        };
        let err = ensure_usable_inner(&def).await.unwrap_err();
        assert!(err.contains("no install spec"), "unexpected error: {err}");
        clear_status(&def.name);
    }
}
