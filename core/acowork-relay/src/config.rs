//! Relay server configuration (design doc 24 §5.1, §5.5).

use std::net::SocketAddr;
use std::path::PathBuf;

/// Runtime configuration for the relay server.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// TCP listener for everything (service domain + device domains).
    pub listen: SocketAddr,
    /// SNI that routes to the control-plane HTTP server (tunnel endpoint,
    /// health, admin API), e.g. `relay.example.com`.
    pub service_domain: String,
    /// SNI suffix routed to device tunnels: `<gw-id>.<suffix>`.
    pub device_domain_suffix: String,
    /// Directory for the persisted device store (public keys).
    pub data_dir: PathBuf,
    /// PEM cert chain. `None` = plain HTTP/WS on `listen` (dev/test only).
    pub tls_cert: Option<PathBuf>,
    /// PEM private key matching `tls_cert`.
    pub tls_key: Option<PathBuf>,
    /// Admin API bearer token. `None` disables the admin API entirely.
    pub admin_token: Option<String>,
    /// When `true`, first-connect (TOFU) registration is rejected — devices
    /// must be pre-registered through the admin API (enterprise mode).
    pub require_registration: bool,

    // ── Limits (§5.5) ──────────────────────────────────────────────
    /// Max concurrent client connections per gw-id.
    pub max_conns_per_gateway: usize,
    /// Max concurrent unauthenticated (pre-handshake) tunnel connects.
    pub max_pending_tunnels: usize,
    /// Max live device tunnels (bound for the registry).
    pub max_tunnels: usize,
    /// Tunnel keepalive interval advertised to the Gateway (seconds).
    pub keepalive_s: u64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:443".parse().unwrap(),
            service_domain: "relay.example.com".into(),
            device_domain_suffix: "relay.example.com".into(),
            data_dir: ".acowork-relay".into(),
            tls_cert: None,
            tls_key: None,
            admin_token: None,
            require_registration: false,
            max_conns_per_gateway: 64,
            max_pending_tunnels: 256,
            max_tunnels: 5_000,
            keepalive_s: 30,
        }
    }
}

impl RelayConfig {
    /// Whether TLS is enabled (cert + key must come as a pair).
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }

    /// Extract the gw-id from a device-domain SNI, if it matches the
    /// configured suffix.
    pub fn gw_id_from_sni(&self, sni: &str) -> Option<String> {
        let sni = sni.trim_end_matches('.').to_ascii_lowercase();
        let suffix = self.device_domain_suffix.trim_end_matches('.').to_ascii_lowercase();
        let prefix = sni.strip_suffix(&suffix)?;
        let gw_id = prefix.strip_suffix('.')?;
        if gw_id.is_empty() || gw_id.contains('.') {
            return None;
        }
        Some(gw_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gw_id_extraction() {
        let cfg = RelayConfig {
            service_domain: "relay.example.com".into(),
            device_domain_suffix: "relay.example.com".into(),
            ..RelayConfig::default()
        };
        assert_eq!(
            cfg.gw_id_from_sni("0f1e2d3c-4b5a-4678-9abc-def012345678.relay.example.com"),
            Some("0f1e2d3c-4b5a-4678-9abc-def012345678".to_string())
        );
        // Trailing dot (FQDN form) and case-insensitivity.
        assert_eq!(
            cfg.gw_id_from_sni("ABC4678-4B5A-4678-9ABC-DEF012345678.Relay.Example.COM."),
            Some("abc4678-4b5a-4678-9abc-def012345678".to_string())
        );
        // Service domain itself is not a device domain.
        assert_eq!(cfg.gw_id_from_sni("relay.example.com"), None);
        // Nested subdomains do not match.
        assert_eq!(cfg.gw_id_from_sni("a.b.relay.example.com"), None);
        // Different suffix.
        assert_eq!(cfg.gw_id_from_sni("abc.other.com"), None);
    }

    #[test]
    fn tls_pair_must_be_complete() {
        let mut cfg = RelayConfig::default();
        assert!(!cfg.tls_enabled());
        cfg.tls_cert = Some("cert.pem".into());
        assert!(!cfg.tls_enabled());
        cfg.tls_key = Some("key.pem".into());
        assert!(cfg.tls_enabled());
    }
}
