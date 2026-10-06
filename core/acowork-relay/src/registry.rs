//! Live tunnel registry and per-gateway limits (design doc 24 §5.4–§5.5).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, Semaphore};

use acowork_core::relay::driver::YamuxDriverHandle;

/// Frames the registry may ask a tunnel's control task to emit.
#[derive(Debug, Clone)]
pub enum ControlOut {
    Goaway { reason: String },
}

/// Handle to one live tunnel, cloneable and stored in the registry.
#[derive(Clone)]
pub struct TunnelHandle {
    pub session_id: String,
    /// Open tagged data streams through the tunnel's driver (the relay
    /// is the stream opener, §5.2 — the tag byte is written by the
    /// driver before the stream is handed over).
    driver: YamuxDriverHandle,
    /// Channel to push a farewell frame onto the control stream (e.g.
    /// GOAWAY on eviction) before the tunnel is torn down.
    goaway_tx: mpsc::Sender<ControlOut>,
    /// Per-gateway concurrent client-connection cap (§5.5).
    conn_permits: Arc<Semaphore>,
    /// Aborting the driver task drops the socket — the tunnel teardown.
    abort: tokio::task::AbortHandle,
}

impl TunnelHandle {
    /// Open a new data stream through the tunnel (the caller acquires a
    /// connection permit first).
    pub async fn open_stream(&self, tag: u8) -> anyhow::Result<yamux::Stream> {
        self.driver.open_stream(Some(tag)).await
    }

    /// Acquire one client-connection permit (§5.5 per-gateway cap).
    pub async fn acquire_conn_permit(&self) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        Ok(self.conn_permits.clone().acquire_owned().await?)
    }

    /// Ask the control task to send a farewell frame, then tear the
    /// tunnel down.
    ///
    /// Teardown is performed by `run_tunnel` (grace sleep → driver
    /// abort) once the control task ends; if the control task was
    /// already gone the send fails silently and `run_tunnel` is on its
    /// own teardown path anyway.
    pub async fn goaway_and_kill(&self, reason: &str) {
        let _ = self
            .goaway_tx
            .send(ControlOut::Goaway {
                reason: reason.to_string(),
            })
            .await;
    }

    /// Kill the tunnel immediately (no GOAWAY): the driver task is
    /// aborted, dropping the socket and every in-flight data stream.
    pub fn kill(&self) {
        self.abort.abort();
    }

    pub fn new(
        session_id: String,
        driver: YamuxDriverHandle,
        goaway_tx: mpsc::Sender<ControlOut>,
        conn_permits: Arc<Semaphore>,
        abort: tokio::task::AbortHandle,
    ) -> Self {
        Self {
            session_id,
            driver,
            goaway_tx,
            conn_permits,
            abort,
        }
    }
}

/// All live tunnels, keyed by gw-id. At most one tunnel per gw-id
/// (single-active, §5.4): registering a new one evicts the old.
pub struct TunnelRegistry {
    tunnels: RwLock<HashMap<String, TunnelHandle>>,
    max_tunnels: usize,
    /// Sliding-window REGISTER attempt timestamps per gw-id (§5.4 rate cap).
    register_attempts: Mutex<HashMap<String, VecDeque<i64>>>,
}

impl TunnelRegistry {
    pub fn new(max_tunnels: usize) -> Self {
        Self {
            tunnels: RwLock::new(HashMap::new()),
            max_tunnels,
            register_attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tunnel. Returns the evicted predecessor (single-active):
    /// the caller is responsible for GOAWAY + teardown, so the old tunnel
    /// observes its eviction instead of just dying.
    ///
    /// Returns `Err` when the tunnel cap is reached.
    pub fn register(
        &self,
        gw_id: &str,
        handle: TunnelHandle,
    ) -> Result<Option<TunnelHandle>, String> {
        let mut tunnels = self.tunnels.write();
        if !tunnels.contains_key(gw_id) && tunnels.len() >= self.max_tunnels {
            return Err(format!(
                "tunnel capacity reached ({} live tunnels)",
                tunnels.len()
            ));
        }
        let old = tunnels.insert(gw_id.to_string(), handle);
        if old.is_some() {
            tracing::info!(gw_id, "evicting previous tunnel (single-active)");
        }
        Ok(old)
    }

    /// Remove a tunnel only if the live entry is still `session_id` — a
    /// dying old tunnel must not unregister its replacement.
    pub fn remove_session(&self, gw_id: &str, session_id: &str) -> bool {
        let mut tunnels = self.tunnels.write();
        if tunnels
            .get(gw_id)
            .is_some_and(|h| h.session_id == session_id)
        {
            tunnels.remove(gw_id);
            true
        } else {
            false
        }
    }

    pub fn get(&self, gw_id: &str) -> Option<TunnelHandle> {
        self.tunnels.read().get(gw_id).cloned()
    }

    /// Live tunnels snapshot for the admin API.
    pub fn list(&self) -> Vec<(String, String)> {
        self.tunnels
            .read()
            .iter()
            .map(|(gw, h)| (gw.clone(), h.session_id.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.tunnels.read().len()
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn is_empty(&self) -> bool {
        self.tunnels.read().is_empty()
    }

    /// Sliding-window REGISTER rate check: at most `max_per_min` attempts
    /// per gw-id per minute. Records the attempt when allowed.
    pub fn check_register_rate(&self, gw_id: &str, max_per_min: usize) -> bool {
        const WINDOW_SECS: i64 = 60;
        let now = chrono::Utc::now().timestamp();
        let mut attempts = self.register_attempts.lock();
        let window = attempts.entry(gw_id.to_string()).or_default();
        while window.front().is_some_and(|t| now - *t >= WINDOW_SECS) {
            window.pop_front();
        }
        if window.len() >= max_per_min {
            return false;
        }
        window.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::relay::driver::spawn_driver;

    /// A handle whose driver runs over a throwaway duplex pipe.
    async fn make_handle(session: &str) -> TunnelHandle {
        use tokio_util::compat::TokioAsyncReadCompatExt as _;
        let (io, _peer) = tokio::io::duplex(64);
        let (driver, _inbound, task) = spawn_driver(
            io.compat(),
            yamux::Config::default(),
            yamux::Mode::Server,
            1,
        );
        let (goaway_tx, _goaway_rx) = mpsc::channel(4);
        TunnelHandle::new(
            session.to_string(),
            driver,
            goaway_tx,
            Arc::new(Semaphore::new(2)),
            task.abort_handle(),
        )
    }

    #[tokio::test]
    async fn single_active_eviction() {
        let registry = TunnelRegistry::new(100);
        let old = registry
            .register("g1", make_handle("s1").await)
            .unwrap();
        assert!(old.is_none());
        let evicted = registry
            .register("g1", make_handle("s2").await)
            .unwrap();
        assert_eq!(evicted.unwrap().session_id, "s1");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("g1").unwrap().session_id, "s2");
    }

    #[tokio::test]
    async fn remove_session_only_own_entry() {
        let registry = TunnelRegistry::new(100);
        registry.register("g1", make_handle("s1").await).unwrap();
        // A stale session id cannot remove the live entry.
        assert!(!registry.remove_session("g1", "s0"));
        assert!(registry.remove_session("g1", "s1"));
        assert!(registry.is_empty());
        // Removing an unknown gw-id is a no-op.
        assert!(!registry.remove_session("g1", "s1"));
    }

    #[test]
    fn register_rate_limit_window() {
        let registry = TunnelRegistry::new(100);
        for _ in 0..3 {
            assert!(registry.check_register_rate("g1", 3));
        }
        assert!(!registry.check_register_rate("g1", 3));
        // Independent budget per gw-id.
        assert!(registry.check_register_rate("g2", 3));
    }

    #[tokio::test]
    async fn tunnel_cap() {
        let registry = TunnelRegistry::new(1);
        registry.register("g1", make_handle("s1").await).unwrap();
        // Replacing an existing entry still fits within the cap.
        assert!(
            registry
                .register("g1", make_handle("s2").await)
                .unwrap()
                .is_some()
        );
        // A second gw-id exceeds it.
        assert!(
            registry
                .register("g2", make_handle("s3").await)
                .is_err()
        );
    }
}
