//! User-profile snapshot sync (ADR-084 §决策 4b).
//!
//! `user_profiles.json` is owned by `acowork-user`; the Gateway only *reads* a
//! pulled copy into `GatewayState.resource_cache.user_profile_list`, which is
//! what `global_resources_builders` turns into the retained
//! `acowork/global/user_profile` topic Runtime's `last_user_profile` comes
//! from (ADR-042). Before ADR-084 the Gateway loaded that file from its own
//! data dir; now it is a peer's state, so it is pulled over HTTP instead.
//!
//! Two entry points, both landing here:
//!
//! 1. **Startup / restart** — [`crate::lifecycle::user_supervisor`] calls
//!    [`refresh`] once the service answers `/health`, so a Gateway boot never
//!    serves a stale identity (its cache starts empty).
//! 2. **Change signal** — the service publishes
//!    `acowork/user/profiles/changed` after every account/profile mutation;
//!    `dispatch` calls [`refresh`] on receipt.
//!
//! Failure is always soft: no service (`user_process == None`, the same
//! condition the proxy answers 503 on) or an unreachable endpoint leaves the
//! previous snapshot in place. A stale identity is better than a blank one,
//! and the next mutation retries.

use acowork_core::protocol::UserProfileListFile;

use crate::lifecycle::user_supervisor::{SharedState, http_client};

/// Pull `GET /internal/user-profiles` and cache it (ADR-084 §决策 4b).
///
/// Returns `true` when the cached snapshot changed — i.e. when a global
/// resource republish was triggered.
pub async fn refresh(state: &SharedState) -> bool {
    let Some(port) = service_port(state).await else {
        tracing::debug!("user service not running; skipping profile snapshot pull");
        return false;
    };

    let Some(list) = pull(port).await else {
        // Endpoint failure already logged inside `pull`.
        return false;
    };

    let trigger = {
        let mut gw = state.write().await;
        let cached = &gw.resource_cache.user_profile_list;
        // Version alone is not enough: a Gateway that just started holds
        // `{version: 0, users: []}`, which is also what a fresh service
        // reports — and skipping there would leave the retained topic at
        // whatever the publisher's first snapshot saw.
        if cached.version == list.version && cached.users.len() == list.users.len() {
            return false;
        }
        tracing::info!(
            version = list.version,
            users = list.users.len(),
            "user profile snapshot refreshed"
        );
        gw.resource_cache.user_profile_list = list;
        // Cloned out of the lock: the publisher loop must never be woken
        // while a `GatewayState` lock is held.
        gw.mqtt_publisher_handle.as_ref().map(|h| h.create_trigger())
    };

    // No MQTT (disabled / not yet started) is not a failure: the cache holds
    // the new list, so the next publish carries it.
    if let Some(t) = trigger {
        t.trigger();
    }
    true
}

/// The port the running service actually bound, if it is up.
async fn service_port(state: &SharedState) -> Option<u16> {
    let gw = state.read().await;
    gw.user_process
        .as_ref()
        .filter(|p| p.ready)
        .map(|p| p.port)
}

/// Fetch and decode the snapshot. `None` on any failure (logged).
async fn pull(port: u16) -> Option<UserProfileListFile> {
    let url = format!("http://127.0.0.1:{port}/internal/user-profiles");
    match http_client().get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<UserProfileListFile>().await {
            Ok(list) => Some(list),
            Err(e) => {
                tracing::warn!(error = %e, "user profile snapshot is not a UserProfileListFile");
                None
            }
        },
        Ok(resp) => {
            tracing::warn!(
                status = %resp.status(),
                "user profile snapshot pull rejected"
            );
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "user profile snapshot pull failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::mqtt_proto::USER_PROFILES_CHANGED_TOPIC;

    /// The topic is a wire contract with the service's publisher; a rename on
    /// one side only would silently stop profile refresh.
    #[test]
    fn topic_matches_the_shared_contract() {
        assert_eq!(USER_PROFILES_CHANGED_TOPIC, "acowork/user/profiles/changed");
    }

    /// No service → no pull, no panic (the 503 regime).
    #[tokio::test]
    async fn no_service_is_a_noop() {
        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::gateway::state::GatewayState::new("/tmp/acowork-profile-sync-test"),
        ));
        assert!(!refresh(&state).await);
    }

    /// The pull half of the chain, against a real HTTP server: this is the
    /// part the MQTT signal cannot cover (a broker that refuses our client
    /// leaves the signal dead), so it is pinned on its own.
    #[tokio::test]
    async fn pulled_snapshot_replaces_the_cache_and_is_idempotent() {
        let served = std::sync::Arc::new(tokio::sync::Mutex::new(
            acowork_core::protocol::UserProfileListFile {
                version: 3,
                users: vec![acowork_core::protocol::UserProfile {
                    user_id: "u-1".to_string(),
                    display_name: "Alice".to_string(),
                    language: "zh-CN".to_string(),
                    timezone: "Asia/Shanghai".to_string(),
                    city: None,
                    country: None,
                    occupation: None,
                    avatar: None,
                    builtin_avatar: None,
                    communication_style: None,
                    custom: Default::default(),
                    created_at: "2026-09-27T00:00:00Z".to_string(),
                    updated_at: "2026-09-27T00:00:00Z".to_string(),
                    is_active: true,
                }],
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler_list = served.clone();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/internal/user-profiles",
                axum::routing::get(move || {
                    let list = handler_list.clone();
                    async move { axum::Json(list.lock().await.clone()) }
                }),
            );
            let _ = axum::serve(listener, app).await;
        });
        // Give the listener a moment to be accepting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::gateway::state::GatewayState::new("/tmp/acowork-profile-sync-test"),
        ));
        state.write().await.user_process = Some(
            crate::lifecycle::user_supervisor::UserProcessState {
                pid: 0,
                port,
                ready: true,
            },
        );

        // First pull: the empty startup cache is replaced.
        assert!(refresh(&state).await);
        {
            let gw = state.read().await;
            assert_eq!(gw.resource_cache.user_profile_list.version, 3);
            assert_eq!(gw.resource_cache.user_profile_list.users.len(), 1);
            assert_eq!(
                gw.resource_cache.user_profile_list.users[0].display_name,
                "Alice"
            );
        }

        // Same version again (duplicate signal): no republish, no churn.
        assert!(!refresh(&state).await);
    }

    /// A cache that already matches the service is left alone: reachable
    /// service, but nothing changed, so no republish is triggered.
    #[tokio::test]
    async fn unchanged_snapshot_does_not_republish() {
        let state: SharedState = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::gateway::state::GatewayState::new("/tmp/acowork-profile-sync-test"),
        ));
        // A service that is "up" but points at a closed port: the pull fails
        // and the cached (empty) list survives untouched.
        state.write().await.user_process = Some(
            crate::lifecycle::user_supervisor::UserProcessState {
                pid: 0,
                port: 1,
                ready: true,
            },
        );
        assert!(!refresh(&state).await);
        let gw = state.read().await;
        assert!(gw.resource_cache.user_profile_list.users.is_empty());
        assert_eq!(gw.resource_cache.user_profile_list.version, 0);
    }
}
