//! Deployment auth-mode resolution (ADR-076 §决策 12).
//!
//! ACowork has a single topology — there is no separate "local" and
//! "remote" code path (see `docs/runbooks/single-machine-remote-topology.md`
//! §0: "没有 local/remote 两套拓扑"). The difference between a single
//! self-hosted run and a multi-user team deployment is *external
//! reachability*, which maps to one knob: `AUTH_MODE`.
//!
//! - [`AuthMode::Local`] — bind loopback. The physical OS user is the trust
//!   boundary: legacy bearer-token auth, no login, no admin role, session
//!   filtering disabled. Every ADR-076 §1-§11 decision is a no-op.
//! - [`AuthMode::MultiUser`] — bind non-loopback. The complete account
//!   system: login tokens, admin role, session isolation, user-user chat.
//!
//! Resolution priority: explicit CLI > explicit TOML > bind-address
//! inference > default [`AuthMode::Local`].

use serde::{Deserialize, Serialize};

/// Which authentication regime the Gateway runs under (ADR-076 §决策 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Single-machine, loopback-bound. Physical OS user management is the
    /// trust boundary; the ADR-076 account system is a no-op.
    Local,
    /// Multi-user / remote. The full ADR-076 account system is active.
    MultiUser,
}

impl AuthMode {
    /// Parse a config/CLI string. Accepts `local`, `multi_user`
    /// (and the `multi-user` spelling for convenience since clap users
    /// often type a hyphen). Case-insensitive; `None` on anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "multi_user" | "multi-user" => Some(Self::MultiUser),
            _ => None,
        }
    }

    /// The canonical lowercase spelling used in config files and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MultiUser => "multi_user",
        }
    }

    /// Whether the full ADR-076 account system is active.
    pub fn is_multi_user(self) -> bool {
        matches!(self, Self::MultiUser)
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Is `host` a loopback bind address (IPv4 `127.0.0.0/8` or IPv6 `::1`)?
///
/// Loopback is the only bind that implies the single-machine trust
/// boundary. The wildcard (`0.0.0.0` / `::`), any routable host (LAN IP,
/// DNS name) and IPv6 link-local (`fe80::/10`) are all treated as reachable
/// by others → [`AuthMode::MultiUser`] (the safe side).
pub fn is_loopback_host(host: &str) -> bool {
    // `--addr` / `[http].host` may arrive bracketed (`[::1]`) or bare.
    let host = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    // A literal hostname: only `localhost` maps to the loopback trust
    // boundary; everything else (a LAN DNS name, a public host) does not.
    host.eq_ignore_ascii_case("localhost")
}

/// Resolve the effective auth mode (ADR-076 §决策 12).
///
/// Priority: explicit CLI > explicit TOML > bind-address inference >
/// default [`AuthMode::Local`].
pub fn resolve_auth_mode(
    cli: Option<AuthMode>,
    toml: Option<AuthMode>,
    bind_host: &str,
) -> AuthMode {
    cli.or(toml).unwrap_or_else(|| {
        if is_loopback_host(bind_host) {
            AuthMode::Local
        } else {
            AuthMode::MultiUser
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_both_spellings() {
        assert_eq!(AuthMode::parse("local"), Some(AuthMode::Local));
        assert_eq!(AuthMode::parse("LOCAL"), Some(AuthMode::Local));
        assert_eq!(AuthMode::parse("multi_user"), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("multi-user"), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("  Multi_User "), Some(AuthMode::MultiUser));
        assert_eq!(AuthMode::parse("nope"), None);
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.5"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));

        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("::"));
        assert!(!is_loopback_host("192.168.1.20"));
        assert!(!is_loopback_host("fe80::1"));
        assert!(!is_loopback_host("gateway.example.com"));
    }

    /// The §7.5 truth table from ADR-076.
    #[test]
    fn resolve_truth_table() {
        // bind inference
        assert_eq!(
            resolve_auth_mode(None, None, "127.0.0.1"),
            AuthMode::Local
        );
        assert_eq!(
            resolve_auth_mode(None, None, "0.0.0.0"),
            AuthMode::MultiUser
        );
        assert_eq!(
            resolve_auth_mode(None, None, "192.168.1.20"),
            AuthMode::MultiUser
        );
        assert_eq!(resolve_auth_mode(None, None, "[::1]"), AuthMode::Local);
        assert_eq!(
            resolve_auth_mode(None, None, "fe80::1"),
            AuthMode::MultiUser,
            "link-local defaults to the safe side"
        );

        // explicit CLI overrides bind
        assert_eq!(
            resolve_auth_mode(Some(AuthMode::Local), None, "0.0.0.0"),
            AuthMode::Local
        );
        assert_eq!(
            resolve_auth_mode(Some(AuthMode::MultiUser), None, "127.0.0.1"),
            AuthMode::MultiUser
        );

        // CLI beats TOML
        assert_eq!(
            resolve_auth_mode(Some(AuthMode::Local), Some(AuthMode::MultiUser), "0.0.0.0"),
            AuthMode::Local
        );
        // TOML beats bind inference
        assert_eq!(
            resolve_auth_mode(None, Some(AuthMode::MultiUser), "127.0.0.1"),
            AuthMode::MultiUser
        );
    }
}
