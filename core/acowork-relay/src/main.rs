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
    acowork_core::logging::init_subprocess_logging("info");
    acowork_core::logging::install_panic_hook();
    let cli = Cli::parse();
    let config = cli.into_config()?;

    let handle = acowork_relay::entry::serve(config).await?;
    handle.stopped().await;
    Ok(())
}
