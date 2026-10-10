//! LSP server registry — the Node-owned runtime file (ADR-091).
//!
//! `{node_home}/lsp_servers.json` is the single runtime truth: the read-only
//! preset shipped with the suite ∪ the user's custom servers ∪ the persisted
//! install state. The LSP relay *reads* this file (as its server list); it
//! never writes it — the Node is the only writer, through this type. That
//! asymmetry is the reason the relay can be restarted on a config change
//! without any state sync protocol (ADR-091 §5.6/§5.8).
//!
//! Three rules the rest of the module exists to enforce:
//!
//! | rule | where |
//! |------|-------|
//! | preset metadata is refreshed from the preset on every merge; the user's `candidate_args` and the install state survive | [`merge_registry`] |
//! | a custom entry whose key collides with a preset key wins, and the collision is reported instead of being silently overwritten | [`merge_registry`] `conflicts` |
//! | every mutation goes through the write lock and lands in one atomic file write | [`LspRegistry::mutate`] |
//!
//! `installing` is deliberately NOT part of the persisted record (D10): an
//! in-flight install is process state, and a crash must not leave the file
//! claiming an install that is running.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use indexmap::IndexMap;
use thiserror::Error;
use tokio::sync::RwLock;

use acowork_core::lsp::{
    LSP_REGISTRY_VERSION, LspInstallRecord, LspInstallState, LspRegistryFile, LspRuntimeEntry,
    LspServerEntry, LspServersConfig, LspSource, load_preset_servers,
};

/// How long a converged probe result stays authoritative before a read
/// triggers a background re-convergence. Mirrors the relay's own status TTL
/// default (`ACOWORK_LSP_STATUS_TTL_SECS`, 1800 s) — the relay caches the
/// probe, this only decides when the Node asks it again.
pub const CONVERGE_TTL: std::time::Duration = std::time::Duration::from_secs(1800);

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid LSP server name {0:?}: use 1-40 chars of [a-z0-9_-], starting with a letter or digit, no dots")]
    InvalidName(String),
    #[error("LSP server {0} already exists")]
    AlreadyExists(String),
    #[error("LSP server {0} not found")]
    NotFound(String),
    #[error("preset LSP server {0} cannot be removed — it comes back from the suite on the next merge")]
    PresetNotRemovable(String),
    #[error("LSP server {0} is already being installed on this node")]
    InstallInFlight(String),
    #[error("registry file error: {0}")]
    Io(String),
}

/// Result of one live probe of a server binary, as reported by the relay
/// (ADR-091 §5.8). Kept as a plain local struct so the Node does not import
/// any relay type — the relay's `/api/lsp/status` JSON is the contract.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub installed: bool,
    pub command: Option<String>,
}

/// Merged view handed to callers: the registry plus the collisions found
/// while merging (surfaced in the UI, never silently dropped).
#[derive(Debug, Clone)]
pub struct RegistrySnapshot {
    pub servers: IndexMap<String, LspRuntimeEntry>,
    pub conflicts: Vec<String>,
}

#[derive(Debug)]
struct Inner {
    servers: IndexMap<String, LspRuntimeEntry>,
    /// Keys whose install is running right now (D10 — memory only).
    installing: HashSet<String>,
    /// Preset/custom key collisions from the last merge.
    conflicts: Vec<String>,
    /// When the probe state was last converged from the relay. `None` until
    /// the first convergence completes.
    last_converged: Option<Instant>,
}

/// The Node's LSP registry: in-memory truth + the runtime file behind it.
///
/// Cheap to clone (the state lives behind one lock), so every HTTP handler
/// gets its own handle (ADR-091 D11).
pub struct LspRegistry {
    inner: RwLock<Inner>,
    home: PathBuf,
    asset_dir: Option<PathBuf>,
    /// Single-flight guard for the §5.8 convergence task: a relay probe is
    /// seconds, and three triggers (ready / install / stale read) can easily
    /// overlap. Held outside `inner` so claiming it never takes the lock.
    converge_in_flight: Arc<AtomicBool>,
}

impl LspRegistry {
    /// Load + merge + persist. Called once at Node startup, before the LSP
    /// relay is spawned — the relay reads the file this writes, so the
    /// write MUST happen first (it also creates the file on a fresh node).
    pub async fn load(home: &Path, asset_dir: Option<PathBuf>) -> Arc<Self> {
        let preset = load_preset_servers(asset_dir.as_deref()).unwrap_or_else(|| {
            tracing::warn!(
                asset_dir = ?asset_dir,
                "No LSP preset registry found — starting with an empty preset (custom servers still work)"
            );
            LspServersConfig {
                version: LSP_REGISTRY_VERSION,
                servers: IndexMap::new(),
            }
        });
        let runtime = read_runtime_file(home).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Ignoring unreadable LSP runtime registry — rebuilding from preset");
            LspRegistryFile::default()
        });
        let (servers, conflicts) = merge_registry(&preset, runtime);
        let registry = Arc::new(Self {
            inner: RwLock::new(Inner {
                servers,
                installing: HashSet::new(),
                conflicts,
                last_converged: None,
            }),
            home: home.to_path_buf(),
            asset_dir,
            converge_in_flight: Arc::new(AtomicBool::new(false)),
        });
        // Rewrite unconditionally: this both materialises the file on a fresh
        // node and applies a preset upgrade (D6) without a second read.
        if let Err(e) = registry.persist_now().await {
            tracing::warn!(error = %e, "Failed to persist the merged LSP registry");
        }
        registry
    }

    /// Path of the runtime file (diagnostics + the P6 redline reference).
    pub fn path(&self) -> PathBuf {
        self.home.join("lsp_servers.json")
    }

    /// Directory holding `lsp_install/` scripts (ADR-091 D16).
    pub fn asset_dir(&self) -> Option<PathBuf> {
        self.asset_dir.clone().or_else(acowork_core::lsp::resolve_lsp_asset_dir)
    }

    /// Preset registry, re-read from the asset dir. The preset is a shipped
    /// file, so a fresh read is cheap and picks up a suite upgrade without a
    /// Node restart; the merge on top of that happens in [`Self::load`].
    pub async fn presets(&self) -> LspServersConfig {
        load_preset_servers(self.asset_dir().as_deref()).unwrap_or(LspServersConfig {
            version: LSP_REGISTRY_VERSION,
            servers: IndexMap::new(),
        })
    }

    pub async fn snapshot(&self) -> RegistrySnapshot {
        let inner = self.inner.read().await;
        RegistrySnapshot {
            servers: inner.servers.clone(),
            conflicts: inner.conflicts.clone(),
        }
    }

    pub async fn get(&self, name: &str) -> Option<LspRuntimeEntry> {
        self.inner.read().await.servers.get(name).cloned()
    }

    pub async fn is_installing(&self, name: &str) -> bool {
        self.inner.read().await.installing.contains(name)
    }

    /// Keys whose install script is running right now — reported alongside
    /// the registry so the UI can render the `installing` state that is
    /// deliberately never persisted (D10).
    pub async fn installing_names(&self) -> Vec<String> {
        let inner = self.inner.read().await;
        let mut names: Vec<String> = inner.installing.iter().cloned().collect();
        names.sort();
        names
    }

    /// Claim the convergence slot. Returns `false` when one is already
    /// running, so a burst of triggers collapses into a single probe.
    pub fn begin_convergence(&self) -> bool {
        !self.converge_in_flight.swap(true, Ordering::AcqRel)
    }

    /// Release the convergence slot (see [`Self::begin_convergence`]).
    pub fn end_convergence(&self) {
        self.converge_in_flight.store(false, Ordering::Release);
    }

    /// Whether the persisted probe state is older than [`CONVERGE_TTL`] —
    /// the trigger for a background convergence on read (§5.8 c).
    pub async fn converge_stale(&self) -> bool {
        let inner = self.inner.read().await;
        match inner.last_converged {
            None => true,
            Some(at) => at.elapsed() > CONVERGE_TTL,
        }
    }

    /// Mark + persist an install as in-flight (D10). Refuses a second
    /// concurrent install of the same key so the UI cannot show two
    /// progress spinners for one script run.
    pub async fn begin_install(&self, name: &str) -> Result<(), RegistryError> {
        let mut inner = self.inner.write().await;
        if !inner.servers.contains_key(name) {
            return Err(RegistryError::NotFound(name.to_string()));
        }
        if !inner.installing.insert(name.to_string()) {
            return Err(RegistryError::InstallInFlight(name.to_string()));
        }
        Ok(())
    }

    /// Record the outcome of an install run and release the in-flight mark.
    /// The output is truncated before it reaches the file (§5.4).
    pub async fn finish_install(
        &self,
        name: &str,
        state: LspInstallState,
        stdout: &str,
        stderr: &str,
    ) -> Result<(), RegistryError> {
        let mut inner = self.inner.write().await;
        inner.installing.remove(name);
        let entry = inner
            .servers
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        let now_ms = now_epoch_ms();
        entry.install.install_state = state;
        entry.install.install_stdout = LspRuntimeEntry::truncate_output(stdout);
        entry.install.install_stderr = LspRuntimeEntry::truncate_output(stderr);
        entry.install.last_install_ms = Some(now_ms);
        if state == LspInstallState::Failed {
            // A failed run invalidates whatever was believed about this key.
            entry.install.installed_command = None;
        }
        self.persist_locked(&inner)
    }

    /// Release the in-flight mark WITHOUT writing a record.
    ///
    /// Only for the error path of [`Self::finish_install`]: if the file write
    /// failed, the key must not stay marked as installing forever — the next
    /// attempt is allowed to run the script again.
    pub async fn abort_install(&self, name: &str) {
        self.inner.write().await.installing.remove(name);
    }

    /// Add a custom server. `source` is set here, not by the caller — a
    /// client cannot smuggle a preset entry in through this path.
    pub async fn add_custom(
        &self,
        name: &str,
        metadata: LspServerEntry,
    ) -> Result<(), RegistryError> {
        validate_server_name(name)?;
        if metadata.candidates.is_empty() {
            return Err(RegistryError::Io(format!(
                "LSP server {name} needs at least one candidate command"
            )));
        }
        let mut inner = self.inner.write().await;
        if inner.servers.contains_key(name) {
            return Err(RegistryError::AlreadyExists(name.to_string()));
        }
        inner.servers.insert(
            name.to_string(),
            LspRuntimeEntry {
                metadata,
                source: LspSource::Custom,
                install: LspInstallRecord::default(),
            },
        );
        self.persist_locked(&inner)
    }

    /// `PUT /lsp/servers/{name}` — update the instance block.
    ///
    /// For a **preset** entry only `candidate_args` is durable: the merge
    /// refreshes the rest of the metadata from the shipped preset on every
    /// start (D6), so accepting other edits here would silently revert them
    /// and lie to the user about what was saved. For a **custom** entry the
    /// whole metadata block is replaced.
    pub async fn update(
        &self,
        name: &str,
        metadata: LspServerEntry,
    ) -> Result<LspRuntimeEntry, RegistryError> {
        let mut inner = self.inner.write().await;
        let entry = inner
            .servers
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        if entry.is_custom() {
            if metadata.candidates.is_empty() {
                return Err(RegistryError::Io(format!(
                    "LSP server {name} needs at least one candidate command"
                )));
            }
            entry.metadata = metadata;
        } else {
            entry.metadata.candidate_args = metadata.candidate_args;
        }
        let updated = entry.clone();
        self.persist_locked(&inner)?;
        Ok(updated)
    }

    /// Remove an entry. The language-server binary is NOT uninstalled
    /// (ADR-091 §5.5) — removing the record is a registry operation, and
    /// uninstalling a third-party binary that other tooling may share is not
    /// this Node's call to make.
    pub async fn delete(&self, name: &str) -> Result<(), RegistryError> {
        let mut inner = self.inner.write().await;
        let entry = inner
            .servers
            .get(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        if !entry.is_custom() {
            return Err(RegistryError::PresetNotRemovable(name.to_string()));
        }
        inner.servers.shift_remove(name);
        self.persist_locked(&inner)
    }

    /// Apply probe results from the relay and persist the converged state
    /// (§5.8). The relay probed; the Node writes.
    pub async fn converge(
        &self,
        results: IndexMap<String, ProbeResult>,
    ) -> Result<(), RegistryError> {
        let mut inner = self.inner.write().await;
        for (name, probe) in results {
            let Some(entry) = inner.servers.get_mut(&name) else {
                continue; // relay knows a key this file no longer has
            };
            match (probe.installed, entry.install.install_state) {
                // Keep `installed` when the probe agrees — it carries the
                // evidence that OUR script installed it, which `detected`
                // would erase.
                (true, LspInstallState::Installed) => {
                    entry.install.installed_command = probe.command;
                }
                (true, _) => {
                    entry.install.install_state = LspInstallState::Detected;
                    entry.install.installed_command = probe.command;
                }
                // Gone from PATH: downgrade to unknown, not `failed` — a
                // mounted volume or a PATH change is not an install failure.
                (false, _) => {
                    entry.install.install_state = LspInstallState::Unknown;
                    entry.install.installed_command = None;
                }
            }
            entry.install.last_check_ms = Some(now_epoch_ms());
        }
        inner.last_converged = Some(Instant::now());
        self.persist_locked(&inner)
    }

    /// Write the current in-memory state to disk. Callers that mutate hold
    /// the write lock and use [`Self::persist_locked`]; this one is for the
    /// startup path.
    async fn persist_now(&self) -> Result<(), RegistryError> {
        let inner = self.inner.read().await;
        write_registry_file(&self.path(), &inner.servers)
    }

    /// Write while the caller already holds the lock (never re-enters the
    /// lock — the registry MUST NOT deadlock on its own save path).
    fn persist_locked(&self, inner: &Inner) -> Result<(), RegistryError> {
        write_registry_file(&self.path(), &inner.servers)
    }

    /// Test/CLI hook: replace the whole in-memory map (used by the
    /// convergence tests to seed a state without touching the preset).
    #[cfg(test)]
    async fn seed(&self, servers: IndexMap<String, LspRuntimeEntry>) {
        self.inner.write().await.servers = servers;
    }
}

/// preset ∪ custom ∪ state (ADR-091 §5.3).
///
/// Returns `(merged, conflicts)`. Ordering: preset keys in preset-file order
/// first, then custom keys in runtime-file order — both `IndexMap`s, so the
/// harness list is stable across restarts.
pub fn merge_registry(
    preset: &LspServersConfig,
    runtime: LspRegistryFile,
) -> (IndexMap<String, LspRuntimeEntry>, Vec<String>) {
    let mut merged: IndexMap<String, LspRuntimeEntry> = IndexMap::new();
    let mut conflicts = Vec::new();

    for (name, preset_entry) in &preset.servers {
        match runtime.servers.get(name) {
            // A preset key that a user took for their own server: the user's
            // entry wins (never silently overwrite someone's config), and the
            // collision is surfaced so it can be resolved by hand.
            Some(existing) if existing.is_custom() => {
                merged.insert(name.clone(), existing.clone());
                conflicts.push(name.clone());
            }
            // Preset entry: refresh metadata, keep what the user owns.
            Some(existing) => {
                merged.insert(
                    name.clone(),
                    LspRuntimeEntry {
                        metadata: LspServerEntry {
                            candidate_args: existing.metadata.candidate_args.clone(),
                            ..preset_entry.clone()
                        },
                        source: LspSource::Preset,
                        install: existing.install.clone(),
                    },
                );
            }
            None => {
                merged.insert(name.clone(), LspRuntimeEntry::from_preset(preset_entry.clone()));
            }
        }
    }

    for (name, entry) in &runtime.servers {
        if entry.is_custom() && !merged.contains_key(name) {
            merged.insert(name.clone(), entry.clone());
        }
    }

    (merged, conflicts)
}

/// Server key rules: it becomes a URL path segment, a JSON key and (for
/// presets) an install-script name stem, so keep it to a conservative slug.
fn validate_server_name(name: &str) -> Result<(), RegistryError> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && !name.contains('.')
            && !name.contains("..")
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok { Ok(()) } else { Err(RegistryError::InvalidName(name.to_string())) }
}

fn read_runtime_file(home: &Path) -> Result<LspRegistryFile, String> {
    let path = home.join("lsp_servers.json");
    if !path.exists() {
        return Ok(LspRegistryFile::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("{}: {e}", path.display()))
}

/// `tmp + fsync + chmod 0600 + rename` (ADR-091 §5.4).
///
/// On Windows the 0600 arm is a no-op — the current-user ACL is already the
/// effective boundary for a file under `%USERPROFILE%`, and the install log
/// this file holds is truncated to the tail (see `truncate_output`), so the
/// residual risk is a token that survived in the last 4 KB. Warn instead of
/// pretending.
fn write_registry_file(
    path: &Path,
    servers: &IndexMap<String, LspRuntimeEntry>,
) -> Result<(), RegistryError> {
    let file = LspRegistryFile {
        version: LSP_REGISTRY_VERSION,
        servers: servers.clone(),
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| RegistryError::Io(e.to_string()))?;

    let parent = path
        .parent()
        .ok_or_else(|| RegistryError::Io(format!("{} has no parent dir", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;

    // Single writer per file name: a fixed `*.tmp` is enough because every
    // write holds the registry write lock.
    let tmp = path.with_extension("json.tmp");
    let write_result = (|| -> Result<(), RegistryError> {
        let mut f = std::fs::File::create(&tmp).map_err(|e| RegistryError::Io(e.to_string()))?;
        std::io::Write::write_all(&mut f, json.as_bytes()).map_err(|e| RegistryError::Io(e.to_string()))?;
        f.sync_all().map_err(|e| RegistryError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| RegistryError::Io(e.to_string()))?;
        }
        #[cfg(windows)]
        {
            tracing::debug!(
                path = %tmp.display(),
                "Windows has no 0600 equivalent here — relying on the per-user ACL"
            );
        }
        std::fs::rename(&tmp, path).map_err(|e| RegistryError::Io(e.to_string()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Convenience handle used by the HTTP layer.
pub type SharedLspRegistry = Arc<LspRegistry>;

/// Minimal metadata block for tests across the `lsp_*` modules — an entry is
/// seven fields wide and only `candidates` is ever load-bearing in a test.
#[cfg(test)]
pub(crate) fn test_entry(candidates: &[&str]) -> LspServerEntry {
    LspServerEntry {
        candidates: candidates.iter().map(|s| s.to_string()).collect(),
        args: vec![],
        candidate_args: Default::default(),
        install_hint: "hint".into(),
        install_script: Some("rust".into()),
        description: "desc".into(),
        root_markers: vec![],
    }
}

/// An empty registry over a temp home, for tests that need a handle but not
/// the preset merge ([`LspRegistry::load`] is async because it persists).
///
/// `asset_dir` is explicit rather than defaulted: left as `None` the lookup
/// falls back to the repository's own `assets/lsp_install/`, so a test
/// exercising the install route would run a real developer script.
#[cfg(test)]
pub(crate) fn for_tests(home: &Path, asset_dir: Option<PathBuf>) -> Arc<LspRegistry> {
    Arc::new(LspRegistry {
        inner: RwLock::new(Inner {
            servers: IndexMap::new(),
            installing: HashSet::new(),
            conflicts: Vec::new(),
            last_converged: None,
        }),
        home: home.to_path_buf(),
        asset_dir,
        converge_in_flight: Arc::new(AtomicBool::new(false)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(candidates: &[&str]) -> LspServerEntry {
        LspServerEntry {
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            args: vec![],
            candidate_args: Default::default(),
            install_hint: "hint".into(),
            install_script: Some("rust".into()),
            description: "desc".into(),
            root_markers: vec!["Cargo.toml".into()],
        }
    }

    fn preset(pairs: &[(&str, &str)]) -> LspServersConfig {
        let mut servers = IndexMap::new();
        for (name, cmd) in pairs {
            servers.insert((*name).to_string(), entry(&[cmd]));
        }
        LspServersConfig {
            version: LSP_REGISTRY_VERSION,
            servers,
        }
    }

    fn runtime(entries: Vec<(String, LspRuntimeEntry)>) -> LspRegistryFile {
        LspRegistryFile {
            version: LSP_REGISTRY_VERSION,
            servers: entries.into_iter().collect(),
        }
    }

    /// D6: metadata is refreshed from the preset, while the user's
    /// `candidate_args` and the persisted install state survive.
    #[test]
    fn merge_refreshes_preset_metadata_but_keeps_user_edits_and_state() {
        let preset = preset(&[("rust", "rust-analyzer")]);
        let mut stored = LspRuntimeEntry::from_preset(entry(&["old-name"]));
        stored.metadata.candidate_args =
            HashMap::from([("old-name".to_string(), vec!["--enable-clippy".to_string()])]);
        stored.install.install_state = LspInstallState::Installed;
        let merged = merge_registry(&preset, runtime(vec![("rust".into(), stored)])).0;

        let rust = &merged["rust"];
        assert_eq!(rust.metadata.candidates, ["rust-analyzer"]); // refreshed
        assert_eq!(rust.metadata.install_hint, "hint"); // refreshed
        assert_eq!(
            rust.metadata.candidate_args["old-name"],
            vec!["--enable-clippy"]
        ); // preserved
        assert_eq!(rust.install.install_state, LspInstallState::Installed); // preserved
    }

    /// B5 (the collision rule): a preset key a user already owns must NOT be
    /// overwritten, and must be reported instead.
    #[test]
    fn merge_keeps_custom_entry_on_key_collision_and_reports_it() {
        let preset = preset(&[("rust", "rust-analyzer")]);
        let mut custom = LspRuntimeEntry {
            metadata: entry(&["my-rust"]),
            source: LspSource::Custom,
            install: LspInstallRecord::default(),
        };
        custom.install.install_state = LspInstallState::Detected;
        let (merged, conflicts) = merge_registry(&preset, runtime(vec![("rust".into(), custom)]));

        assert_eq!(merged["rust"].metadata.candidates, ["my-rust"]);
        assert_eq!(merged["rust"].source, LspSource::Custom);
        assert_eq!(merged["rust"].install.install_state, LspInstallState::Detected);
        assert_eq!(conflicts, vec!["rust".to_string()]);
    }

    /// Ordering invariant from §5.3: preset order first, then custom.
    #[test]
    fn merge_orders_presets_then_customs() {
        let preset = preset(&[("go", "gopls"), ("rust", "rust-analyzer")]);
        let mut custom_a = LspRuntimeEntry {
            metadata: entry(&["a-lsp"]),
            source: LspSource::Custom,
            install: LspInstallRecord::default(),
        };
        custom_a.metadata.description = "a".into();
        let merged = merge_registry(
            &preset,
            runtime(vec![
                ("a".into(), custom_a),
                (
                    "b".into(),
                    LspRuntimeEntry {
                        metadata: entry(&["b-lsp"]),
                        source: LspSource::Custom,
                        install: LspInstallRecord::default(),
                    },
                ),
            ]),
        )
        .0;
        let keys: Vec<&String> = merged.keys().collect();
        assert_eq!(keys, vec!["go", "rust", "a", "b"]);
    }

    #[test]
    fn name_rules_reject_path_and_url_tricks() {
        let too_long = "a".repeat(41);
        for bad in ["", "Upper", "-lead", "with.dot", too_long.as_str(), "with/slash"] {
            assert!(
                validate_server_name(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        for good in ["rust", "my-lsp", "team_lsp2"] {
            assert!(validate_server_name(good).is_ok(), "expected {good:?} to pass");
        }
    }

    #[tokio::test]
    async fn registry_round_trips_through_disk_and_rebuilds_on_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let preset_dir = tmp.path().join("assets");
        std::fs::create_dir_all(&preset_dir).unwrap();
        std::fs::write(
            preset_dir.join("lsp_servers.json"),
            serde_json::to_vec(&preset(&[("rust", "rust-analyzer")])).unwrap(),
        )
        .unwrap();

        let home = tmp.path().join("node-home");
        let reg = LspRegistry::load(&home, Some(preset_dir.clone())).await;
        assert!(reg.path().exists(), "startup must materialise the file");
        assert!(reg.get("rust").await.is_some());

        reg.add_custom("my-lsp", entry(&["my-lsp"])).await.unwrap();
        let on_disk = read_runtime_file(&home).unwrap();
        assert!(on_disk.servers.contains_key("my-lsp"));

        // A restart merges: the custom entry survives, the preset is refreshed.
        drop(reg);
        let reg2 = LspRegistry::load(&home, Some(preset_dir)).await;
        assert!(reg2.get("my-lsp").await.is_some());
        assert_eq!(reg2.get("my-lsp").await.unwrap().source, LspSource::Custom);
        assert_eq!(reg2.snapshot().await.conflicts, Vec::<String>::new());
    }

    #[tokio::test]
    async fn add_rejects_duplicates_and_delete_refuses_presets() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = LspRegistry::load(tmp.path(), None).await;
        reg.add_custom("my-lsp", entry(&["my-lsp"])).await.unwrap();
        assert!(matches!(
            reg.add_custom("my-lsp", entry(&["other"])).await,
            Err(RegistryError::AlreadyExists(_))
        ));
        assert!(matches!(
            reg.delete("no-such-server").await,
            Err(RegistryError::NotFound(_))
        ));
        // A custom entry is removable — that is the whole point of `source`.
        reg.delete("my-lsp").await.unwrap();
        assert!(reg.get("my-lsp").await.is_none());
        // nothing shipped here, so seed a preset-shaped entry by hand
        let mut servers = reg.snapshot().await.servers;
        servers.insert("rust".into(), LspRuntimeEntry::from_preset(entry(&["rust-analyzer"])));
        reg.seed(servers).await;
        assert!(matches!(
            reg.delete("rust").await,
            Err(RegistryError::PresetNotRemovable(_))
        ));
    }

    /// D10: an in-flight install is never written, and a second one is refused.
    #[tokio::test]
    async fn install_in_flight_is_memory_only_and_serialised() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = LspRegistry::load(tmp.path(), None).await;
        reg.add_custom("my-lsp", entry(&["my-lsp"])).await.unwrap();

        reg.begin_install("my-lsp").await.unwrap();
        assert!(reg.is_installing("my-lsp").await);
        assert!(
            read_runtime_file(&home_of(&reg))
                .unwrap()
                .servers["my-lsp"]
                .install
                .install_stdout
                .is_empty()
        );
        assert!(matches!(
            reg.begin_install("my-lsp").await,
            Err(RegistryError::InstallInFlight(_))
        ));

        reg.finish_install("my-lsp", LspInstallState::Installed, "done", "")
            .await
            .unwrap();
        assert!(!reg.is_installing("my-lsp").await);
        let on_disk = read_runtime_file(&home_of(&reg)).unwrap();
        assert_eq!(on_disk.servers["my-lsp"].install.install_state, LspInstallState::Installed);
        // `installing` cannot even be expressed in the persisted enum.
        let json = serde_json::to_string(&on_disk).unwrap();
        assert!(!json.contains("installing"));
    }

    fn home_of(reg: &LspRegistry) -> PathBuf {
        reg.path().parent().unwrap().to_path_buf()
    }

    /// §5.8: a probe that says "not there" downgrades to `unknown`, and an
    /// existing `installed` is kept (our script installed it — `detected`
    /// would erase that evidence).
    #[tokio::test]
    async fn converge_maps_probe_results_without_losing_installed_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = LspRegistry::load(tmp.path(), None).await;
        reg.add_custom("keep", entry(&["keep-lsp"])).await.unwrap();
        reg.add_custom("gone", entry(&["gone-lsp"])).await.unwrap();
        reg.finish_install("keep", LspInstallState::Installed, "ok", "")
            .await
            .unwrap();

        let mut results = IndexMap::new();
        results.insert(
            "keep".to_string(),
            ProbeResult {
                installed: true,
                command: Some("keep-lsp".into()),
            },
        );
        results.insert(
            "gone".to_string(),
            ProbeResult {
                installed: false,
                command: None,
            },
        );
        reg.converge(results).await.unwrap();

        let snap = reg.snapshot().await;
        assert_eq!(snap.servers["keep"].install.install_state, LspInstallState::Installed);
        assert_eq!(snap.servers["gone"].install.install_state, LspInstallState::Unknown);
        assert!(snap.servers["gone"].install.last_check_ms.is_some());
        assert!(!reg.converge_stale().await);
    }

    /// The file the relay reads must stay parseable as a preset-shaped
    /// config — the whole Node-writes/relay-reads split depends on it.
    #[tokio::test]
    async fn written_file_is_relay_compatible() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = LspRegistry::load(tmp.path(), None).await;
        reg.add_custom("my-lsp", entry(&["my-lsp"])).await.unwrap();
        let raw = std::fs::read_to_string(reg.path()).unwrap();
        let as_preset: LspServersConfig = serde_json::from_str(&raw).unwrap();
        assert_eq!(as_preset.servers["my-lsp"].candidates, ["my-lsp"]);
    }
}
