//! LSP install-script execution on the Node (ADR-091 D15).
//!
//! The logic was the LSP relay's (`acowork-lsp-relay/src/install.rs`, whose
//! HTTP routes are removed by this ADR). It is **copied** here rather than
//! shared: the Node MUST NOT depend on the relay crate, and the relay is a
//! different process on a different machine in a split deployment, so the
//! executor has to live where the filesystem and the PATH do.
//!
//! What changed on the way over:
//!
//! - script discovery is keyed on the **preset asset dir** (`ACOWORK_LSP_ASSET_DIR`,
//!   D16), not on `ACOWORK_LSP_CONFIG_DIR` — that one now names `{node_home}`,
//!   where install scripts never live;
//! - the outcome is not just an HTTP response: the caller writes it into the
//!   runtime registry (`install_state` / `install_stdout` / `last_install_ms`),
//!   which is the whole point of moving install behind the Node;
//! - a successful install does NOT restart the relay. Newly installed binaries
//!   become visible through a forced probe (the relay refreshes its profile
//!   PATH cache on `?force=true`), so the editor's LSP WebSocket survives an
//!   install (§5.10).
//!
//! Timeout model (unchanged from the relay): the script is killed only after
//! 60 s of *no output*, so a slow but progressing `npm install` finishes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use acowork_core::process::run_command_with_idle_timeout;

/// Idle timeout applied to every install script (see module docs).
pub const INSTALL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What an install run produced. Mirrors the relay's response fields so the
/// UI contract survives the move, plus the reason a run never started.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Script exited 0.
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// `Some` when the script could not be run at all (spawn failure) or was
    /// killed for idling — the message the UI shows instead of a log tail.
    pub failure: Option<String>,
}

impl InstallOutcome {
    fn spawn_failed(e: &acowork_core::process::IdleTimeoutError) -> Self {
        Self {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: e.stderr().to_string(),
            failure: Some(format!("Failed to run install script: {}", e.stderr())),
        }
    }

    fn timed_out(e: &acowork_core::process::IdleTimeoutError) -> Self {
        Self {
            success: false,
            exit_code: None,
            stdout: e.stdout().to_string(),
            stderr: e.stderr().to_string(),
            failure: Some(format!(
                "Install script timed out after {}s with no output. \
                 The process may be stuck — please retry or install manually.",
                e.idle_secs
            )),
        }
    }
}

/// Script file name for a registry `install_script` stem, per platform
/// (`"rust"` → `rust.ps1` on Windows, `rust.sh` elsewhere).
///
/// Rejects anything that could escape the `lsp_install/` directory: the stem
/// is user-supplied for a custom server (behind the Gateway's `can_manage`
/// gate, but a managed node is still not a place to accept `../../x`), and it
/// is concatenated into a path here.
pub fn script_filename(stem: &str) -> Result<String, String> {
    let stem = stem.trim();
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains("..")
        || std::path::Path::new(stem).extension().is_some()
    {
        return Err(format!("invalid install script name: {stem:?}"));
    }
    let ext = if cfg!(windows) { "ps1" } else { "sh" };
    Ok(format!("{stem}.{ext}"))
}

/// Candidate `lsp_install/` directories, in priority order.
///
/// The explicit asset dir (what the Desktop injected) comes first, then the
/// shared fallback chain from `acowork_core::lsp` — the same chain the preset
/// is read from, so the registry and the scripts can never be resolved from
/// two different suites. `exe_dir` / cwd are last because a packaged build
/// puts `lsp_install/` next to the binary.
fn install_dir_candidates(asset_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = asset_dir {
        dirs.push(dir.to_path_buf());
    }
    dirs.extend(acowork_core::lsp::lsp_asset_dir_candidates());
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        dirs.push(exe_dir.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    dirs
}

/// Locate `{dir}/lsp_install/{filename}`, first hit wins.
pub fn find_install_script(asset_dir: Option<&Path>, filename: &str) -> Option<PathBuf> {
    install_dir_candidates(asset_dir)
        .into_iter()
        .map(|dir| dir.join("lsp_install").join(filename))
        .find(|path| path.is_file())
}

/// Read a script for the `GET /lsp/servers/{name}/script` preview (D14: the
/// UI shows what it is about to execute before executing it).
pub fn load_install_script(asset_dir: Option<&Path>, filename: &str) -> Option<(PathBuf, String)> {
    let path = find_install_script(asset_dir, filename)?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Some((path, content)),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read install script");
            None
        }
    }
}

/// Run the script with the idle-timeout watchdog.
///
/// Never takes the registry lock — the caller records the outcome afterwards.
pub async fn run_install_script(path: &Path) -> InstallOutcome {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-ExecutionPolicy", "Bypass", "-NoProfile", "-File"]);
        c.arg(path);
        c
    } else {
        let mut c = tokio::process::Command::new("bash");
        c.arg(path);
        c
    };

    match run_command_with_idle_timeout(&mut cmd, INSTALL_IDLE_TIMEOUT).await {
        Ok(output) => InstallOutcome {
            success: output.exit_code == Some(0),
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            failure: None,
        },
        // `idle_secs == 0` is the sentinel the shared helper uses for "the
        // process never started" (spawn/await failure), not a real timeout.
        Err(e) if e.idle_secs == 0 => InstallOutcome::spawn_failed(&e),
        Err(e) => InstallOutcome::timed_out(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_filename_uses_the_platform_extension() {
        let name = script_filename("rust").unwrap();
        let expected = if cfg!(windows) { "rust.ps1" } else { "rust.sh" };
        assert_eq!(name, expected);
    }

    /// The traversal guard: an `install_script` is data in a file the user
    /// can edit, and it becomes a path component here.
    #[test]
    fn script_filename_rejects_path_tricks() {
        for bad in ["", "   ", "..", "../evil", "a/b", "a\\b", "rust.sh", "x..y"] {
            assert!(script_filename(bad).is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[tokio::test]
    async fn missing_script_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            find_install_script(Some(tmp.path()), "definitely-not-here.sh").is_none(),
            "an empty asset dir must resolve to nothing"
        );
    }

    #[tokio::test]
    async fn script_is_found_in_the_explicit_asset_dir_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lsp_install");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake-lsp.sh");
        std::fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
        let found = find_install_script(Some(tmp.path()), "fake-lsp.sh").unwrap();
        assert_eq!(found, path);
    }

    /// A real script run, so the exit-code mapping AND the platform runner
    /// (powershell on Windows, bash elsewhere) are exercised rather than
    /// asserted by inspection. This is the coverage the relay's install
    /// integration tests carried before ADR-091 D15 moved the executor here.
    #[tokio::test]
    async fn run_reports_exit_code_and_output() {
        let tmp = tempfile::tempdir().unwrap();
        let (ext, ok_body, bad_body) = if cfg!(windows) {
            (
                "ps1",
                "[Console]::Out.WriteLine('hello')\r\nexit 0\r\n",
                "[Console]::Error.WriteLine('boom')\r\nexit 3\r\n",
            )
        } else {
            (
                "sh",
                "#!/bin/sh\necho hello\nexit 0\n",
                "#!/bin/sh\necho boom >&2\nexit 3\n",
            )
        };

        let ok = tmp.path().join(format!("ok.{ext}"));
        std::fs::write(&ok, ok_body).unwrap();
        let good = run_install_script(&ok).await;
        assert!(
            good.success,
            "stdout: {} stderr: {} failure: {:?}",
            good.stdout,
            good.stderr,
            good.failure
        );
        assert_eq!(good.exit_code, Some(0));
        assert!(good.stdout.contains("hello"), "stdout: {}", good.stdout);
        assert!(good.failure.is_none());

        let bad = tmp.path().join(format!("bad.{ext}"));
        std::fs::write(&bad, bad_body).unwrap();
        let failed = run_install_script(&bad).await;
        assert!(!failed.success);
        assert_eq!(failed.exit_code, Some(3));
        assert!(
            format!("{}{}", failed.stderr, failed.stdout).contains("boom"),
            "stderr: {} stdout: {}",
            failed.stderr,
            failed.stdout
        );
    }
}
