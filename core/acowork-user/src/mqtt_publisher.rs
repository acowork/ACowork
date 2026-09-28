//! Profile-change signal publisher (`acowork/user/profiles/changed`, QoS 1).
//!
//! Event-driven alternative to the Gateway polling us: every mutation of the
//! account store goes through this process (REST handlers share the same
//! `profiles::rebuild_and_save_user_profile_cache`), so after the write lands
//! we publish one signal. The Gateway listens, re-pulls
//! `GET /internal/user-profiles` and republishes its global resources — the
//! `last_user_profile` Runtime sees (ADR-042 / ADR-084 §决策 4b).
//!
//! Delivery contract (same shape as `acowork-doc`'s tree-change publisher):
//! - QoS 1, **non-retained**, plain-text payload: the new profile-list
//!   version. A lost signal is healed by the next mutation and by the
//!   Gateway's startup pull, so this cannot wedge the Runtime identity.
//! - Publisher is a **global singleton** so the profile code can emit
//!   without threading a handle through `AppState`. Unit tests never call
//!   [`init`], so [`notify_profiles_changed`] is a no-op there.
//! - Publish failure is fire-and-forget: warn and drop. A business write
//!   (create user, rename, change avatar) must never fail because the broker
//!   is down.
//!
//! Reuse: [`MqttClient`] from `acowork-mqtt-session` (ADR-065) gives us the
//! whole poll loop — reconnect, backoff, wake recovery — for free; this
//! process only ever publishes, never subscribes.

use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use rumqttc::QoS;
use tracing::warn;

use acowork_core::mqtt_proto::USER_PROFILES_CHANGED_TOPIC;
use acowork_mqtt_session::{MqttClient, MqttClientConfig, MqttClientError, MqttClientHandler};

/// Broker client id. The shared prefix `user:service` is matched by
/// the Gateway broker allowlist (ADR-084 §决策 4b, `starts_with`).
/// The per-process suffix avoids `Duplicate client_id, dropping previous`
/// on internal reconnect — rumqttd drops the previous session and the
/// publisher's reconnect loop then races itself; a unique suffix makes
/// each reconnect a fresh session and the broker stops issuing
/// `Duplicate`. Stable across restarts of the same process is not
/// required (the broker auth path treats all `user:service:*` ids
/// identically and the rest of the runtime never subscribes to this
/// publisher).
fn client_id() -> String {
    format!("user:service:{}", std::process::id())
}

static PUBLISHER: OnceLock<Arc<UserMqttPublisher>> = OnceLock::new();

/// Initialise the global publisher. Called once from `main`; a broker that
/// is not up yet is fine — [`MqttClient::connect`] only spawns the poll task
/// (auto-reconnect inside), it does not wait for CONNACK.
///
/// `password` is the Gateway publisher token when `mqtt.auth_enabled` is on
/// (ADR-084 §决策 4b); `None` connects without credentials.
///
/// Never returns an error that should kill the user service: without the
/// signal the Gateway still picks the new profiles up on its next restart.
pub async fn init(host: &str, port: u16, password: Option<String>) {
    match UserMqttPublisher::connect(host.to_string(), port, password).await {
        Ok(publisher) => {
            let _ = PUBLISHER.set(Arc::new(publisher));
            tracing::info!(host, port, "user MQTT publisher ready");
        }
        Err(e) => {
            warn!(host, port, error = %e,
                "user MQTT publisher init failed — profile-change signals disabled \
                 (Gateway picks changes up on its next pull)");
        }
    }
}

/// Fire-and-forget profile-change notification. No-op when the publisher was
/// never initialised (unit tests) or the broker is unreachable.
pub fn notify_profiles_changed(version: u64) {
    // Service-layer writers are sync (`rebuild_and_save_user_profile_cache`),
    // and a sync unit test has no reactor — where the publisher is
    // uninitialised anyway, so there is nothing to publish.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(publish_profiles_changed(version));
}

async fn publish_profiles_changed(version: u64) {
    let Some(publisher) = PUBLISHER.get() else {
        return;
    };
    publisher.publish_profiles_changed(version).await;
}

/// Publisher: a `MqttClient` with a no-op handler (publish-only).
struct UserMqttPublisher {
    client: MqttClient<NoopHandler>,
}

impl UserMqttPublisher {
    async fn connect(
        host: String,
        port: u16,
        password: Option<String>,
    ) -> Result<Self, MqttClientError> {
        let mut config = MqttClientConfig::new(client_id(), host, port);
        // With `mqtt.auth_enabled` on, the broker only admits
        // `user:service:*` (any per-process id with this prefix) with
        // the Gateway's publisher token; the supervisor forwards it
        // at spawn time (ADR-084 §决策 4b).
        config.credentials = password.map(|p| (client_id(), p));
        let client = MqttClient::connect(config, NoopHandler, None).await?;
        Ok(Self { client })
    }

    /// Publish the profile-list version that a mutation just produced.
    ///
    /// The version is the whole payload: the Gateway logs it and only pulls
    /// when it differs from what it caches, so a duplicate signal costs
    /// nothing.
    async fn publish_profiles_changed(&self, version: u64) {
        if let Err(e) = self
            .client
            .publish_raw(
                USER_PROFILES_CHANGED_TOPIC,
                version_payload(version),
                QoS::AtLeastOnce,
                false,
            )
            .await
        {
            warn!(topic = USER_PROFILES_CHANGED_TOPIC, version, error = %e,
                "user: failed to publish profile-changed signal");
        }
    }
}

/// Publish-only handler: no subscriptions, nothing to do on ConnAck /
/// disconnect / error beyond the defaults.
#[derive(Default)]
struct NoopHandler;

#[async_trait]
impl MqttClientHandler for NoopHandler {}

/// Plain ASCII decimal — see the topic docs: the payload is a hint the
/// Gateway string-compares, not a parsed envelope.
fn version_payload(version: u64) -> Vec<u8> {
    version.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The topic is a wire contract with the Gateway's subscription list; a
    /// rename on one side only would silently stop profile refresh.
    #[test]
    fn topic_matches_the_shared_contract() {
        assert_eq!(
            USER_PROFILES_CHANGED_TOPIC,
            "acowork/user/profiles/changed"
        );
    }

    /// The version is serialised as plain ASCII digits — the Gateway parses
    /// the payload on its side and a JSON wrapper would break it.
    #[test]
    fn version_payload_is_plain_ascii() {
        assert_eq!(version_payload(42), b"42");
        assert_eq!(version_payload(0), b"0");
    }
}
