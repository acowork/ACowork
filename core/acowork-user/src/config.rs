//! acowork-user service configuration.
//!
//! The Gateway resolves the deployment mode and hands it down as
//! `--auth-mode`; everything else the service needs is its own (data
//! directory, port, password policy, bootstrap admin, registration flag).
//!
//! Config precedence (highest first):
//!
//! 1. CLI flags (`--data-dir`, `--port`, `--auth-mode`)
//! 2. TOML config file (`--config <path>`, default `./acowork-user.toml`)
//! 3. [`UserServiceConfig::default`]
//!
//! The data directory resolves to `$HOME/.acowork/acowork-user/`, peer of
//! `acowork-gateway/`, `acowork-node/`, `acowork-pm/` and `acowork-doc/`
//! (ADR-084 §3 goal 2).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::service::{BootstrapAdmin, PasswordPolicy};

/// Which authentication regime the service runs under (ADR-076 §决策 12).
///
/// Mirrors the Gateway's `AuthMode`; the two spellings below are the CLI
/// contract between the Gateway supervisor and this process, so they must
/// stay in sync (see the `auth_mode_spellings_are_the_cli_contract` test).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Single-machine, loopback-bound. Profiles and avatars only: no
    /// accounts, no tokens, no user-to-user chat (ADR-084 §决策 6).
    Local,
    /// The full account system: login / refresh, admin role, user chat.
    MultiUser,
}

impl AuthMode {
    /// Parse the `--auth-mode` value. Accepts `local` / `multi_user`
    /// (plus the `multi-user` hyphen spelling), case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "multi_user" | "multi-user" => Some(Self::MultiUser),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MultiUser => "multi_user",
        }
    }

    /// Whether the account system (login / tokens / chat) is active.
    ///
    /// Under [`AuthMode::Local`] the account routes are **not registered**
    /// at all — an unregistered route cannot be reached by a later
    /// middleware mistake (ADR-076 §决策 12).
    pub fn is_multi_user(self) -> bool {
        matches!(self, Self::MultiUser)
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// acowork-user service runtime config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserServiceConfig {
    /// Data root: accounts, `user_profiles.json`, `users/` (chat trees),
    /// `assets/avatars/`, `auth/ed25519.{key,pub}`.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Standalone-process HTTP listen port. Default `18083`. On conflict the
    /// port auto-increments (max +20); the actual bound port is reported via
    /// `--port-file` to the Gateway supervisor.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Gate for the process itself. The Gateway also gates the spawn via
    /// `[user].enabled`.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Deployment mode handed down by the Gateway.
    #[serde(default = "default_auth_mode")]
    pub auth_mode: AuthMode,

    /// First-boot administrator (ADR-076 §决策 5). Required when the account
    /// store is empty under `multi_user`; ignored once any account exists.
    #[serde(default)]
    pub bootstrap_admin: Option<BootstrapAdmin>,

    /// Password policy enforced on change / bootstrap (ADR-076 §决策 6).
    #[serde(default)]
    pub password_policy: PasswordPolicy,

    /// Whether non-admins may self-register (ADR-076 §决策 6). Default
    /// `false`: only an admin creates accounts.
    #[serde(default)]
    pub registration_open: bool,
}

impl Default for UserServiceConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            port: default_port(),
            enabled: default_true(),
            auth_mode: default_auth_mode(),
            bootstrap_admin: None,
            password_policy: PasswordPolicy::default(),
            registration_open: false,
        }
    }
}

impl UserServiceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("port must be non-zero".to_string());
        }
        Ok(())
    }

    /// Whether the account system is active for this run.
    pub fn is_multi_user(&self) -> bool {
        self.auth_mode.is_multi_user()
    }

    /// Load a TOML config file, layering it over the defaults.
    pub fn load_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read config {}: {e}", path.display()))?;
        toml::from_str(&raw).map_err(|e| format!("invalid config {}: {e}", path.display()))
    }
}

/// Default data directory: `$HOME/.acowork/acowork-user/` (ADR-084 §决策 5).
///
/// `ACOWORK_HOME` wins when set. That variable is what the Gateway's
/// `--home` writes into its own environment before spawning us, so a Gateway
/// booted with `--home X` keeps the whole install — including this store —
/// under `X` instead of reaching into the operator's real home. Without it
/// (the normal case) this is exactly the path the ADR specifies.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ACOWORK_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("acowork-user");
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".acowork").join("acowork-user"))
        .unwrap_or_else(|_| PathBuf::from("./data/acowork-user"))
}

fn default_port() -> u16 {
    18083
}

fn default_true() -> bool {
    true
}

fn default_auth_mode() -> AuthMode {
    AuthMode::Local
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Gateway resolves the mode and forwards `AuthMode::as_str()` on
    /// the supervisor command line. If either side changes a spelling the
    /// other silently falls back to `local` — which turns the account
    /// system off. Pin them.
    #[test]
    fn auth_mode_spellings_are_the_cli_contract() {
        assert_eq!(AuthMode::Local.as_str(), "local");
        assert_eq!(AuthMode::MultiUser.as_str(), "multi_user");
        assert_eq!(AuthMode::parse("multi_user"), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("multi-user"), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("MULTI_USER"), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("local"), Some(AuthMode::Local));
        assert_eq!(AuthMode::parse("nonsense"), None);
    }

    #[test]
    fn data_dir_is_a_peer_of_the_other_services() {
        let dir = UserServiceConfig::default().data_dir;
        assert!(dir.ends_with("acowork-user"), "got {}", dir.display());
        assert!(
            dir.parent().is_some_and(|p| p.ends_with(".acowork")),
            "data dir must sit beside acowork-gateway/ (ADR-084 §3): {}",
            dir.display()
        );
    }

    #[test]
    fn default_port_matches_the_adr() {
        assert_eq!(UserServiceConfig::default().port, 18083);
    }
}
