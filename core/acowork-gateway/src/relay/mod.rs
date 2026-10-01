//! Cloud relay client (design doc `24-cloud-relay-remote-access` §8.2).
//!
//! The Gateway-side half of the relay topology: one outbound WSS tunnel
//! to an acowork-relay server, multiplexing remote-client connections
//! (device-domain byte pipes) onto local listeners by stream tag. The
//! relay itself never parses user traffic — all authentication stays
//! here (§8.4).
//!
//! M3 adds the remote-origin loopback listener pair (§7.2): the strict
//! HTTP listener (guard + `/mqtt` bridge) is hosted in
//! [`remote_listener`]; the strict MQTT listener lives with the broker
//! (`mqtt::RemoteMqttListener`).

pub mod client;
pub mod identity;
pub mod remote_listener;

pub use client::{RelayClient, RelayClientStatus};
pub use identity::RelayIdentity;
pub use remote_listener::{remote_origin_guard, start_remote_http_listener};
