//! acowork-user standalone-process CLI (ADR-084 §决策 5).
//!
//! Mirrors the PM/doc supervisor contract: `--host/--port/--port-file` for
//! binding and port reporting, `--data-dir` for storage, `--auth-mode` for
//! the deployment mode the Gateway resolved, `--gateway-health-url` for the
//! ADR-018 watchdog, and `--config` for the TOML overlay.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::AuthMode;

#[derive(Debug, Parser)]
#[command(
    name = "acowork-user",
    about = "Account / user-domain service (accounts, profiles, avatars, user chat)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Bind host. Must be loopback: the service trusts Gateway-injected
    /// `X-Auth-*` headers (ADR-084 §决策 7).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// HTTP listen port (default 18083; auto-increments on conflict by 20).
    #[arg(long)]
    pub port: Option<u16>,

    /// Write the actually-bound port here (read by the Gateway supervisor).
    #[arg(long)]
    pub port_file: Option<PathBuf>,

    /// Data directory. Default `$HOME/.acowork/acowork-user/`.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Deployment mode, resolved by the Gateway (`local` | `multi_user`).
    #[arg(long)]
    pub auth_mode: Option<String>,

    /// TOML config file to layer over the defaults.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Gateway health URL (ADR-018): self-exit when the Gateway is gone.
    #[arg(long)]
    pub gateway_health_url: Option<String>,

    /// Gateway health probe interval (ms, default 10000 = 10s).
    #[arg(long, default_value = "10000")]
    pub gateway_health_interval_ms: u64,

    /// Gateway unreachable self-exit timeout (ms, default 300000 = 5min).
    #[arg(long, default_value = "300000")]
    pub gateway_health_timeout_ms: u64,

    /// MQTT broker host (the embedded Gateway broker; default 127.0.0.1).
    #[arg(long, default_value = "127.0.0.1")]
    pub mqtt_host: String,

    /// MQTT broker port (default 19875).
    #[arg(long, default_value_t = 19875)]
    pub mqtt_port: u16,

    /// Log level.
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Seed the first administrator (`multi_user` only), then exit.
    AdminSetup {
        /// Read the password from this file.
        #[arg(long)]
        password_file: Option<PathBuf>,

        /// Read the password from stdin (for piping from a secret store).
        #[arg(long)]
        password_stdin: bool,
    },
}

/// Apply the CLI overrides onto a config, validating `--auth-mode`.
pub fn resolve_auth_mode(cli: &Cli, fallback: AuthMode) -> Result<AuthMode, String> {
    match cli.auth_mode.as_deref() {
        Some(raw) => AuthMode::parse(raw)
            .ok_or_else(|| format!("invalid --auth-mode `{raw}` (expected `local` | `multi_user`)")),
        None => Ok(fallback),
    }
}
