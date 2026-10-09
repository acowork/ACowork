//! Local network interface enumeration for the frontend connectivity
//! guard ([localNetwork.ts](../../../src/lib/connectivity/localNetwork.ts)).
//!
//! The guard needs to answer one question cheaply and offline: "is the
//! persisted Gateway URL host one of THIS machine's own addresses?"
//! That single fact decides whether a Wi-Fi hop / wake-from-sleep turns
//! the URL into a stale black hole (gateway still on this machine) or
//! leaves a genuinely remote gateway untouched. Kept in Rust because
//! the webview cannot enumerate interfaces.

use std::net::Ipv4Addr;

/// Keep only the addresses that identify this machine on a network.
///
/// Pure so the filtering rules stay unit-testable without touching the
/// host's real interfaces. Loopback and unspecified addresses are
/// skipped: the connectivity guard already treats loopback URLs as
/// exempt (it is the preferred destination, never a "foreign" host).
fn filter_local_ipv4(addrs: impl Iterator<Item = Ipv4Addr>) -> Vec<String> {
    addrs
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .collect()
}

/// This machine's current non-loopback IPv4 addresses.
///
/// Enumeration failures degrade to an empty list: the frontend guard
/// treats "no addresses" as "cannot decide" and simply does nothing.
#[tauri::command]
pub fn get_local_ipv4_addresses() -> Vec<String> {
    let addrs = match if_addrs::get_if_addrs() {
        Ok(addrs) => addrs,
        Err(e) => {
            tracing::warn!(target: "netinfo", "interface enumeration failed: {e}");
            return Vec::new();
        }
    };
    filter_local_ipv4(addrs.into_iter().filter_map(|a| match a.addr {
        if_addrs::IfAddr::V4(v4) => Some(v4.ip),
        if_addrs::IfAddr::V6(_) => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_drops_loopback_and_unspecified() {
        let got = filter_local_ipv4(
            [
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::new(192, 168, 17, 113),
                Ipv4Addr::UNSPECIFIED,
            ]
            .into_iter(),
        );
        assert_eq!(got, vec!["192.168.17.113".to_string()]);
    }

    #[test]
    fn filter_preserves_multiple_lan_addresses() {
        let got = filter_local_ipv4(
            [
                Ipv4Addr::new(10, 0, 0, 4),
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::new(192, 168, 0, 101),
            ]
            .into_iter(),
        );
        assert_eq!(got, vec!["10.0.0.4".to_string(), "192.168.0.101".to_string()]);
    }
}
