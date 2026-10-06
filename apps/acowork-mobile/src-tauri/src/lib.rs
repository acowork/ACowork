//! ACowork Mobile — Tauri v2 native shell.
//!
//! Deliberately near-empty, and that is the design, not an omission.
//! Desktop's backend spawns and supervises a local Gateway, owns the system
//! tray, and runs the LSP relay sidecar. A phone has none of those: the
//! Gateway it talks to is remote, there is no tray, and the LSP relay is
//! Desktop-only (see ADR-086 §v1 排除项).
//!
//! So the mobile crate owns **no** platform concerns beyond hosting the
//! webview. Every capability it will eventually need — Gateway address,
//! auth token, last-opened tab — is a small piece of app state that
//! `tauri-plugin-store` persists. When the first genuinely native need
//! arrives (push notifications, deep links, camera), it is added here as
//! one audited command rather than as a speculative framework.

/// Where the mobile app last connected. Persisted so a cold start can skip
/// the Gateway-picker screen; `None` means "ask the user".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayEndpoint {
    pub url: String,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .run(tauri::generate_context!())
        .expect("error while running ACowork Mobile");
}

/// Standalone crate: the `[workspace]` key in Cargo.toml keeps it out of the
/// core resolver, exactly like `apps/acowork-desktop/src-tauri`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_round_trips() {
        let e = GatewayEndpoint {
            url: "http://192.168.1.10:19876".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: GatewayEndpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, e.url);
    }
}
