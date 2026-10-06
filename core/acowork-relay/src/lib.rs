//! acowork-relay — cloud relay server (design doc 24 v0.2).
//!
//! A thin relay that lets Desktop/Mobile clients on the public internet
//! reach a Gateway behind NAT, by carrying outbound WSS tunnels that
//! Gateways open to it:
//!
//! - **Service domain** (`relay.example.com`): axum HTTP server — the
//!   Gateway tunnel endpoint (`GET /tunnel`, WS upgrade), health, and the
//!   admin API.
//! - **Device domains** (`<gw-id>.relay.example.com`): TLS SNI-routed raw
//!   byte pipes — one yamux stream per client connection, forwarded through
//!   the Gateway's outbound tunnel. No HTTP parsing, no MQTT knowledge.
//!
//! The relay stores only per-device public keys (Ed25519 TOFU enrollment);
//! all user authentication happens at the Gateway.

pub mod config;
pub mod device_store;
pub mod entry;
pub mod registry;
pub mod tunnel;

pub use config::RelayConfig;
pub use device_store::DeviceStore;
pub use registry::TunnelRegistry;
