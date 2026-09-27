//! acowork-user — standalone account / user-domain service (ADR-084).
//!
//! Lifecycle is managed by the Gateway supervisor (spawn / health-poll /
//! restart). Binds loopback only: it trusts the `X-Auth-*` headers the
//! Gateway's `user_proxy` injects (ADR-084 §决策 7).
//!
//! Public contract: the Desktop still calls `{gateway}/api/auth/*`,
//! `{gateway}/api/users/*` and `{gateway}/api/user/avatar-*` — byte-identical
//! to before the extraction, so the Desktop needed no changes.

use std::io::{BufRead, Read};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use acowork_user::cli::{Cli, Command};
use acowork_user::config::UserServiceConfig;
use acowork_user::server::UserService;
use acowork_user::AuthService;

fn main() {
    let cli = Cli::parse();

    acowork_core::logging::init_subprocess_logging(&cli.log_level);
    acowork_core::logging::install_panic_hook();

    // Config: defaults <- TOML <- CLI.
    let mut config = match &cli.config {
        Some(path) => match UserServiceConfig::load_file(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "failed to load acowork-user config");
                std::process::exit(1);
            }
        },
        None => UserServiceConfig::default(),
    };
    if let Some(dir) = cli.data_dir.clone() {
        config.data_dir = dir;
    }
    if let Some(port) = cli.port {
        config.port = port;
    }
    match acowork_user::cli::resolve_auth_mode(&cli, config.auth_mode) {
        Ok(mode) => config.auth_mode = mode,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
    if let Err(e) = config.validate() {
        tracing::error!(error = %e, "invalid acowork-user config");
        std::process::exit(1);
    }

    if !config.enabled {
        tracing::warn!("acowork-user disabled via config (enabled=false) — exiting");
        std::process::exit(0);
    }

    // `admin-setup` is a one-shot: seed the first admin, then exit. It runs
    // before any server work so it never races the serving path.
    if let Some(Command::AdminSetup {
        password_file,
        password_stdin,
    }) = &cli.command
    {
        let code = run_admin_setup(
            &config,
            password_file.as_deref(),
            *password_stdin,
        );
        std::process::exit(code);
    }

    let auth = if config.is_multi_user() {
        match AuthService::new(
            &config.data_dir,
            config.password_policy.clone(),
            config.bootstrap_admin.clone(),
        ) {
            Ok(svc) => {
                if let Err(e) = svc.ensure_bootstrap_admin() {
                    tracing::error!(error = %e, "failed to ensure bootstrap admin");
                    std::process::exit(1);
                }
                Some(Arc::new(svc))
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to initialize the account service");
                std::process::exit(1);
            }
        }
    } else {
        tracing::info!("AUTH_MODE=local — serving profiles and avatars only");
        None
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to build the tokio runtime");
            std::process::exit(1);
        });

    runtime.block_on(async move {
        if let Some(ref url) = cli.gateway_health_url {
            spawn_gateway_health_watchdog(
                url.clone(),
                Duration::from_millis(cli.gateway_health_interval_ms),
                Duration::from_millis(cli.gateway_health_timeout_ms),
            );
            tracing::info!(url = %url, "Gateway health watchdog started (ADR-018)");
        }

        let service = Arc::new(UserService::new(config.clone(), auth));

        let bind = format!("{}:{}", cli.host, config.port);
        let bind_addr: std::net::SocketAddr = match bind.parse() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(bind = %bind, error = %e, "invalid bind address");
                std::process::exit(1);
            }
        };

        let addr = match service.clone().serve(bind_addr).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "failed to start acowork-user");
                std::process::exit(1);
            }
        };
        tracing::info!(
            addr = %addr,
            data_dir = %config.data_dir.display(),
            auth_mode = %config.auth_mode,
            "acowork-user service listening (standalone)"
        );

        // The Gateway supervisor reads the *actual* port from here (the
        // configured port may have been occupied).
        if let Some(path) = &cli.port_file {
            if let Err(e) = std::fs::write(path, addr.port().to_string()) {
                tracing::error!(path = %path.display(), error = %e, "failed to write port file");
                std::process::exit(1);
            }
            tracing::info!(path = %path.display(), port = addr.port(), "port file written");
        }

        let shutdown = acowork_core::shutdown::Shutdown::new();
        acowork_core::shutdown::install_signal_handlers(shutdown.clone());
        while !shutdown.is_shutting_down() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::info!("acowork-user shut down");
    });
}

/// Seed the first administrator, then exit.
///
/// Returns the process exit code.
fn run_admin_setup(
    config: &UserServiceConfig,
    password_file: Option<&std::path::Path>,
    password_stdin: bool,
) -> i32 {
    if !config.is_multi_user() {
        eprintln!(
            "admin-setup requires --auth-mode multi_user (resolved: {}).",
            config.auth_mode
        );
        return 1;
    }
    let svc = match AuthService::new(&config.data_dir, config.password_policy.clone(), None) {
        Ok(svc) => svc,
        Err(e) => {
            eprintln!("failed to initialize the account service: {e}");
            return 1;
        }
    };
    // Never seed through the bootstrap path — the bootstrap credential is
    // first-boot only and must not become a standing second admin.
    if let Err(e) = svc.ensure_bootstrap_admin() {
        eprintln!("failed to prepare the admin account: {e}");
        return 1;
    }
    let mut password = match read_admin_password(password_file, password_stdin) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let result = svc.set_admin_password(&password);
    // The Argon2id hash is the only durable artifact; drop the plaintext.
    zeroize::Zeroize::zeroize(&mut password);
    match result {
        Ok(()) => {
            println!("Admin password set.");
            0
        }
        Err(e) => {
            eprintln!("failed to set the admin password: {e}");
            1
        }
    }
}

/// Read the admin password from a file or stdin.
///
/// ponytail: the interactive TTY prompt is **not** ported in M1 — the
/// Gateway's own `admin-setup` still owns that path (it is deleted in M4),
/// and only one process should be prompting. M4 must bring the `rpassword`
/// prompt over here; until then the non-interactive paths below are the
/// contract, which is also the one the supervisor/scripts use.
fn read_admin_password(
    password_file: Option<&std::path::Path>,
    password_stdin: bool,
) -> Result<String, String> {
    if let Some(path) = password_file {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("failed to open password file '{}': {e}", path.display()))?;
        // A password past 1 KiB is operator error, not a password.
        let mut buf = String::new();
        file.take(1024)
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read password file '{}': {e}", path.display()))?;
        return Ok(buf.trim_end_matches(['\n', '\r']).to_string());
    }
    if password_stdin {
        let stdin = std::io::stdin();
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("failed to read password from stdin: {e}"))?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_string());
    }
    Err("no password given. Pass --password-file <path> or --password-stdin.".to_string())
}

/// Self-exit when the Gateway stops answering (ADR-018).
///
/// A supervised subprocess that outlives its supervisor becomes an orphan
/// holding a port and a data directory, and the next boot has to fight it.
fn spawn_gateway_health_watchdog(health_url: String, interval: Duration, timeout: Duration) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build HTTP client for the Gateway health probe");

        // The Gateway must be unreachable for `timeout` *continuously* — a
        // restart or a long pause must not take this process with it.
        let mut last_success = std::time::Instant::now();
        loop {
            tokio::time::sleep(interval).await;

            let healthy = match client.get(&health_url).send().await {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            };

            if healthy {
                last_success = std::time::Instant::now();
            } else if last_success.elapsed() >= timeout {
                tracing::error!(
                    elapsed_secs = last_success.elapsed().as_secs(),
                    timeout_secs = timeout.as_secs(),
                    "Gateway unreachable — self-exiting (ADR-018)"
                );
                std::process::exit(0);
            }
        }
    });
}
