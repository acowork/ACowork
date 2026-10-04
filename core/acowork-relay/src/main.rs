//! acowork-relay binary entry point.

use clap::Parser;

use acowork_relay::config::RelayConfig;

#[derive(Parser, Debug)]
#[command(name = "acowork-relay", about = "ACowork cloud relay server (design doc 24)")]
struct Cli {
    /// Listen address, e.g. 0.0.0.0:443.
    #[arg(long, default_value = "0.0.0.0:443")]
    listen: String,

    /// Relay service domain (SNI match for the control-plane HTTP server).
    #[arg(long, default_value = "relay.example.com")]
    service_domain: String,

    /// Device-domain suffix: <gw-id>.<suffix> routes to tunnels.
    #[arg(long, default_value = "relay.example.com")]
    device_domain_suffix: String,

    /// Data directory (device records persisted here).
    #[arg(long, default_value = ".acowork-relay")]
    data_dir: String,

    /// TLS cert chain (PEM). Omit for plain HTTP/WS (dev/test only).
    #[arg(long)]
    tls_cert: Option<String>,

    /// TLS private key (PEM).
    #[arg(long)]
    tls_key: Option<String>,

    /// Admin API bearer token. Required to use the admin API.
    #[arg(long)]
    admin_token: Option<String>,

    /// Reject first-connect (TOFU) registrations unless the device was
    /// pre-registered via the admin API (enterprise deployments).
    #[arg(long, default_value = "false")]
    require_registration: bool,

    /// Log level (trace|debug|info|warn|error). RUST_LOG overrides this.
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Max size in MB per log file before rolling to a new one (0 = default 10).
    #[arg(long, default_value = "10")]
    log_file_size_mb: u64,

    /// Max number of rolling log files to keep (0 = unlimited).
    #[arg(long, default_value = "5")]
    log_file_count: u64,
}

impl Cli {
    fn into_config(self) -> anyhow::Result<RelayConfig> {
        let listen = self
            .listen
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid listen address '{}': {}", self.listen, e))?;
        // NOTE: `service_domain == device_domain_suffix` is legal — SNI
        // matching then decides control-plane vs device by exact match
        // first (see `route_service_or_device`).
        Ok(RelayConfig {
            listen,
            service_domain: self.service_domain,
            device_domain_suffix: self.device_domain_suffix,
            data_dir: self.data_dir.into(),
            tls_cert: self.tls_cert.map(Into::into),
            tls_key: self.tls_key.map(Into::into),
            admin_token: self.admin_token,
            require_registration: self.require_registration,
            ..RelayConfig::default()
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_dir = std::path::Path::new(&cli.data_dir).join("logs");
    init_logging(
        &cli.log_level,
        log_dir.clone(),
        cli.log_file_size_mb,
        cli.log_file_count,
    );
    acowork_core::logging::install_panic_hook();
    let config = cli.into_config()?;

    tracing::info!(
        listen = %config.listen,
        service_domain = %config.service_domain,
        data_dir = %config.data_dir.display(),
        log_dir = %log_dir.display(),
        "acowork-relay starting"
    );

    let handle = acowork_relay::entry::serve(config).await?;
    handle.stopped().await;
    Ok(())
}

/// Initialize tracing to a size-rolling log file under `<data_dir>/logs/`
/// only (no stderr duplicate — journald would just mirror the file).
/// Reuses `acowork_core::logging::SizeRollingFileAppender`, same pattern as
/// Gateway/Node. If the log file cannot be opened (read-only fs, bad perms,
/// full disk) it falls back to stderr-only so startup never aborts, and
/// emits the failure to stderr where journald can still catch it.
fn init_logging(level: &str, log_dir: std::path::PathBuf, size_mb: u64, count: u64) {
    use acowork_core::logging::ChronoLocalTimer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = acowork_core::logging::build_env_filter(level);

    let file_appender = match acowork_core::logging::SizeRollingFileAppender::new(
        log_dir,
        if size_mb > 0 { size_mb } else { 10 },
        count as usize,
    ) {
        Ok(appender) => appender,
        Err(e) => {
            eprintln!("WARN: cannot open relay log file: {e}; falling back to stderr-only");
            let stderr_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_ansi(false)
                .with_timer(ChronoLocalTimer)
                .compact();
            tracing_subscriber::registry()
                .with(env_filter)
                .with(stderr_layer)
                .init();
            return;
        }
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(false)
        .with_timer(ChronoLocalTimer);

    tracing_subscriber::registry().with(env_filter).with(file_layer).init();
}
