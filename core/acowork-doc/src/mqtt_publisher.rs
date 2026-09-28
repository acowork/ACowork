//! Doc tree-change event publisher (`acowork/doc/tree/changed`, QoS 1).
//!
//! Event-driven alternative to polling: every structural mutation of the
//! doc library (dir/doc create / rename / move / delete, trash restore)
//! goes **through this process** (REST API + MCP tools share the same
//! `service` layer), so after the write commits we publish one
//! `DocTreeChangedEvent`. The Desktop listens and force-refreshes the
//! tree layers it has expanded — no filesystem watcher needed (unlike
//! the Runtime's WorkspaceFsWatcher, which must catch edits made by
//! external tools the Runtime never sees).
//!
//! Delivery contract:
//! - QoS 1 (at-least-once), non-retained — a lost event desyncs the
//!   Desktop tree until the next 30s poll or the reconnect full-sync.
//! - Publisher is a **global singleton** so the service layer can emit
//!   without threading a handle through every constructor (which would
//!   churn `DocState` + all service impls + their tests). Unit tests
//!   never call [`init`], so [`publish_tree_changed`] is a no-op there.
//! - Publish failure is fire-and-forget: warn and drop. Business writes
//!   must never block on a broker that is down.
//!
//! Reuse: [`MqttClient`] from `acowork-mqtt-session` (ADR-065) gives us
//! the whole poll loop — reconnect, backoff, wake recovery — for free;
//! the doc process only ever publishes, never subscribes.

use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use prost::Message;
use rumqttc::QoS;
use tracing::warn;

use acowork_core::mqtt_proto::data_envelope;
use acowork_core::mqtt_proto::{DataEnvelope, DocTreeChangedEvent};
use acowork_mqtt_session::{MqttClient, MqttClientConfig, MqttClientError, MqttClientHandler};

/// Fixed topic for doc tree-change events (single doc process per
/// Gateway, so no instance id in the path — unlike agent topics).
pub const DOC_TREE_CHANGED_TOPIC: &str = "acowork/doc/tree/changed";

/// Broker client id. The shared prefix `doc:service` is matched by
/// the Gateway broker allowlist (ADR-084 §决策 4b, `starts_with`).
/// The per-process suffix avoids `Duplicate client_id, dropping previous`
/// on internal reconnect — see [`acowork-user`'s `client_id`] for the
/// same pattern. The doc process is single-instance per Gateway today
/// but the suffix keeps the contract uniform if a future spawns more.
fn client_id() -> String {
    format!("doc:service:{}", std::process::id())
}

static PUBLISHER: OnceLock<Arc<DocMqttPublisher>> = OnceLock::new();

/// Initialise the global publisher. Called once from `main`; a broker
/// that is not up yet is fine — [`MqttClient::connect`] only spawns the
/// poll task (auto-reconnect inside), it does not wait for CONNACK.
/// `password` is the Gateway publisher token when `mqtt.auth_enabled`
/// is on (ADR-084 §决策 4b); `None` connects without credentials.
/// Never returns an error that should kill the doc process.
pub async fn init(host: &str, port: u16, password: Option<String>) {
    match DocMqttPublisher::connect(host.to_string(), port, password).await {
        Ok(publisher) => {
            let _ = PUBLISHER.set(Arc::new(publisher));
            tracing::info!(host, port, "doc MQTT publisher ready");
        }
        Err(e) => {
            warn!(host, port, error = %e,
                "doc MQTT publisher init failed — tree-change events disabled (30s poll fallback)");
        }
    }
}

/// Fire-and-forget tree-change notification. No-op when the publisher
/// was never initialised (unit tests) or the broker is unreachable.
pub async fn publish_tree_changed(changed_dirs: Vec<String>) {
    let Some(publisher) = PUBLISHER.get() else { return };
    publisher.publish_tree_changed(changed_dirs).await;
}

/// Spawned variant for service-layer call sites: business writes must
/// never block on the broker, so emit in a background task. Unit-test
/// no-op (publisher uninitialised → the task returns immediately).
pub fn notify_tree_changed(changed_dirs: Vec<String>) {
    tokio::spawn(publish_tree_changed(changed_dirs));
}

/// Publisher: a `MqttClient` with a no-op handler (publish-only).
pub struct DocMqttPublisher {
    client: MqttClient<DocNoopHandler>,
}

impl DocMqttPublisher {
    pub async fn connect(
        host: String,
        port: u16,
        password: Option<String>,
    ) -> Result<Self, MqttClientError> {
        let mut config = MqttClientConfig::new(client_id(), host, port);
        // With `mqtt.auth_enabled` on, the broker only admits
        // `doc:service:*` (any per-process id with this prefix) with
        // the Gateway's publisher token; the supervisor forwards it
        // at spawn time (ADR-084 §决策 4b).
        config.credentials = password.map(|p| (client_id(), p));
        let client = MqttClient::connect(config, DocNoopHandler, None).await?;
        Ok(Self { client })
    }

    /// Encode and publish a `DocTreeChangedEvent` (QoS 1, non-retained).
    pub async fn publish_tree_changed(&self, changed_dirs: Vec<String>) {
        let envelope = DataEnvelope {
            version: 1,
            payload: Some(data_envelope::Payload::DocTreeChanged(
                DocTreeChangedEvent { changed_dirs },
            )),
        };
        let bytes = envelope.encode_to_vec();
        if let Err(e) = self
            .client
            .publish_raw(DOC_TREE_CHANGED_TOPIC, bytes, QoS::AtLeastOnce, false)
            .await
        {
            warn!(topic = DOC_TREE_CHANGED_TOPIC, error = %e,
                "doc: failed to publish tree-changed event");
        }
    }
}

/// Publish-only handler: no subscriptions, nothing to do on ConnAck /
/// disconnect / error beyond the defaults.
#[derive(Default)]
struct DocNoopHandler;

#[async_trait]
impl MqttClientHandler for DocNoopHandler {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire payload must decode back as a DocTreeChangedEvent with
    /// the same dir scope (regression: wrong envelope field / topic
    /// drift would silently break the Desktop listener).
    #[test]
    fn envelope_roundtrip_preserves_changed_dirs() {
        let envelope = DataEnvelope {
            version: 1,
            payload: Some(data_envelope::Payload::DocTreeChanged(
                DocTreeChangedEvent {
                    changed_dirs: vec!["root".into(), "dir-1".into()],
                },
            )),
        };
        let bytes = envelope.encode_to_vec();
        let decoded = DataEnvelope::decode(bytes.as_slice()).expect("decode roundtrip");
        let Some(data_envelope::Payload::DocTreeChanged(ev)) = decoded.payload else {
            panic!("payload is not DocTreeChangedEvent");
        };
        assert_eq!(ev.changed_dirs, vec!["root", "dir-1"]);
        assert_eq!(DOC_TREE_CHANGED_TOPIC, "acowork/doc/tree/changed");
    }
}
