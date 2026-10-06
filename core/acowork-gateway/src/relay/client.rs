//! Gateway relay client (design doc 24 §5.2–§5.4, §8.2).
//!
//! Maintains the outbound WSS tunnel to an acowork-relay server:
//!
//! ```text
//! supervise ─┬─ run_tunnel_once ─┬─ connect (WSS, /tunnel)
//!            │                   ├─ handshake (REGISTER→CHALLENGE→PROOF→REGISTERED)
//!            │                   ├─ steady state:
//!            │                   │    · relay-opened streams → tag byte → local listener
//!            │                   │    · Ping/Pong keepalive (the tunnel is outbound,
//!            │                   │      so the Gateway owns NAT keepalive)
//!            │                   └─ teardown (Deregister + grace + abort)
//!            └─ exponential backoff reconnect (1s → 60s, reset on success)
//! ```
//!
//! All user authentication stays Gateway-side (§8.4): the relay is a
//! byte pipe; remote login/refresh requests arrive through the tunnel
//! and hit the remote-origin HTTP listener like any other request.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use serde::Serialize;
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::compat::TokioAsyncReadCompatExt as _;

use acowork_core::relay::driver::{copy_bidirectional, spawn_driver};
use acowork_core::relay::proto::{
    read_control_frame_known, write_control_frame, ControlFrame, PROTO_VERSION, encode_pubkey,
    sign_nonce,
};
use acowork_core::relay::ws_stream::WsByteStream;
use acowork_core::relay::{PIPE_IDLE_TIMEOUT, STREAM_TAG_HTTP, STREAM_TAG_MQTT, TEARDOWN_GRACE};

use super::identity::RelayIdentity;

/// How long a single connect+handshake attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Reconnect backoff bounds (seconds).
const BACKOFF_MIN_SECS: u64 = 1;
const BACKOFF_MAX_SECS: u64 = 60;

/// Pause after a clean GOAWAY before reconnecting (the relay is telling
/// us we were superseded — hammering it immediately gains nothing).
const GOAWAY_RECONNECT_PAUSE: Duration = Duration::from_secs(2);

/// How long [`RelayClient::disable`] waits for the supervisor to finish
/// its graceful teardown (Deregister + [`TEARDOWN_GRACE`]) before
/// aborting it. Comfortably above the teardown cost; a wedged task must
/// not hang the disable endpoint.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

// ── Status ──────────────────────────────────────────────────────────

/// Runtime status snapshot for `GET /api/relay/status`.
#[derive(Debug, Clone, Serialize)]
pub struct RelayClientStatus {
    /// Whether the tunnel is wanted (enable/disable state, not liveness).
    pub enabled: bool,
    /// The relay service endpoint in use.
    pub relay_url: Option<String>,
    /// This Gateway's device identifier (stable across restarts).
    pub gw_id: Option<String>,
    /// Whether the tunnel is currently registered with the relay.
    pub connected: bool,
    /// Relay-assigned session id of the current/last tunnel.
    pub session_id: Option<String>,
    /// Advertised keepalive interval (seconds), from REGISTERED.
    pub keepalive_s: Option<u64>,
    /// Last failure reason (cleared on the next successful handshake).
    pub last_error: Option<String>,
    /// When the current tunnel was established (RFC 3339).
    pub connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Default)]
struct StatusInner {
    enabled: bool,
    relay_url: Option<String>,
    gw_id: Option<String>,
    connected: bool,
    session_id: Option<String>,
    keepalive_s: Option<u64>,
    last_error: Option<String>,
    connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl StatusInner {
    fn snapshot(&self) -> RelayClientStatus {
        RelayClientStatus {
            enabled: self.enabled,
            relay_url: self.relay_url.clone(),
            gw_id: self.gw_id.clone(),
            connected: self.connected,
            session_id: self.session_id.clone(),
            keepalive_s: self.keepalive_s,
            last_error: self.last_error.clone(),
            connected_at: self.connected_at,
        }
    }
}

type StatusCell = std::sync::RwLock<StatusInner>;

// ── Client handle ───────────────────────────────────────────────────

/// Runtime manager for the relay tunnel.
///
/// Cheap to construct (no tasks until enabled). `enable`/`disable` are
/// serialized by an internal mutex; the supervisor task owns connection
/// lifecycle and reconnects autonomously until disabled or replaced.
pub struct RelayClient {
    identity_path: PathBuf,
    /// Where `STREAM_TAG_HTTP` streams are forwarded (the remote-origin
    /// loopback listener, §7.2 — created by the remote listener module).
    http_target: SocketAddr,
    status: Arc<StatusCell>,
    supervisor: Mutex<Option<SupervisorHandle>>,
}

struct SupervisorHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl RelayClient {
    /// Build the client. The identity file is created lazily on first
    /// `enable` (no side effects at construction — safe for tests and
    /// for deployments that never turn the relay on).
    pub fn new(identity_path: PathBuf, http_target: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            identity_path,
            http_target,
            status: Arc::new(std::sync::RwLock::new(StatusInner::default())),
            supervisor: Mutex::new(None),
        })
    }

    /// Start (or restart toward a new URL) the tunnel supervisor.
    ///
    /// The identity is loaded-or-minted here; the relay pins whatever
    /// public key it sees first (TOFU, §5.4).
    pub async fn enable(self: &Arc<Self>, url: String) -> Result<(), String> {
        Self::validate_url(&url)?;
        let identity = RelayIdentity::load_or_create(&self.identity_path)?;

        let mut guard = self.supervisor.lock().await;
        // Same URL and already running → idempotent no-op.
        {
            let status = self.status.read().unwrap();
            if status.enabled
                && status.relay_url.as_deref() == Some(url.as_str())
                && guard.is_some()
            {
                return Ok(());
            }
        }
        // Different URL or stale supervisor: stop the old one first.
        if let Some(old) = guard.take() {
            let _ = old.shutdown.send(true);
            let _ = old.task.await;
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let gw_id = identity.gw_id.clone();
        let task = tokio::spawn(supervise(
            identity,
            url.clone(),
            self.http_target,
            shutdown_rx,
            self.status.clone(),
        ));
        *guard = Some(SupervisorHandle {
            shutdown: shutdown_tx,
            task,
        });
        let mut status = self.status.write().unwrap();
        status.enabled = true;
        status.relay_url = Some(url);
        status.gw_id = Some(gw_id);
        status.connected = false;
        status.session_id = None;
        status.keepalive_s = None;
        status.last_error = None;
        status.connected_at = None;
        Ok(())
    }

    /// Stop the tunnel supervisor. The current connection sends a
    /// graceful Deregister and is torn down.
    pub async fn disable(&self) {
        let mut guard = self.supervisor.lock().await;
        if let Some(sup) = guard.take() {
            let _ = sup.shutdown.send(true);
            // Await the supervisor so the graceful teardown (Deregister +
            // TEARDOWN_GRACE) is COMPLETE when the API returns: a
            // following `enable()` must never briefly run two supervisors
            // against the relay. Bounded — a wedged task is aborted past
            // the deadline (dropping the socket closes the tunnel anyway).
            let mut task = sup.task;
            if tokio::time::timeout(SHUTDOWN_WAIT, &mut task).await.is_err() {
                tracing::warn!("relay supervisor did not exit within shutdown wait; aborting");
                task.abort();
            }
        }
        let mut status = self.status.write().unwrap();
        status.enabled = false;
        status.connected = false;
        status.session_id = None;
        status.keepalive_s = None;
        status.connected_at = None;
    }

    /// Current status snapshot.
    pub fn status(&self) -> RelayClientStatus {
        self.status.read().unwrap().snapshot()
    }

    fn validate_url(url: &str) -> Result<(), String> {
        let scheme_ok = url.starts_with("wss://") || url.starts_with("ws://");
        if !scheme_ok {
            return Err("relay url must start with wss:// (or ws:// for local testing)".into());
        }
        if url.len() > 2048 {
            return Err("relay url is too long".into());
        }
        Ok(())
    }
}

// ── Supervisor ──────────────────────────────────────────────────────

/// Reconnect loop: one `run_tunnel_once` at a time, exponential backoff
/// on failures, immediate reconnect after a clean GOAWAY, clean exit on
/// shutdown.
async fn supervise(
    identity: RelayIdentity,
    url: String,
    http_target: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
    status: Arc<StatusCell>,
) {
    let mut backoff = BACKOFF_MIN_SECS;
    loop {
        if *shutdown.borrow() {
            break;
        }
        {
            let mut s = status.write().unwrap();
            s.gw_id = Some(identity.gw_id.clone());
            s.connected = false;
        }
        match run_tunnel_once(&identity, &url, http_target, &mut shutdown, &status).await {
            TunnelEnd::Shutdown => break,
            TunnelEnd::Goaway(reason) => {
                tracing::info!(reason = %reason, "relay sent GOAWAY; will reconnect");
                if sleep_or_shutdown(GOAWAY_RECONNECT_PAUSE, &mut shutdown).await.is_shutdown() {
                    break;
                }
                backoff = BACKOFF_MIN_SECS;
            }
            TunnelEnd::Failed(error) => {
                tracing::warn!(error = %error, "relay tunnel failed; reconnecting with backoff");
                {
                    let mut s = status.write().unwrap();
                    s.connected = false;
                    s.last_error = Some(error);
                }
                if sleep_or_shutdown(Duration::from_secs(backoff), &mut shutdown)
                    .await
                    .is_shutdown()
                {
                    break;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
            }
        }
    }
    let mut s = status.write().unwrap();
    s.connected = false;
    s.session_id = None;
    s.connected_at = None;
}

/// Sleep `duration`, but wake early when shutdown fires.
async fn sleep_or_shutdown(
    duration: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> WakeReason {
    tokio::select! {
        _ = tokio::time::sleep(duration) => WakeReason::Timeout,
        _ = shutdown.changed() => {
            if *shutdown.borrow() {
                WakeReason::Shutdown
            } else {
                WakeReason::Timeout
            }
        }
    }
}

enum WakeReason {
    Timeout,
    Shutdown,
}

impl WakeReason {
    fn is_shutdown(&self) -> bool {
        matches!(self, WakeReason::Shutdown)
    }
}

enum TunnelEnd {
    /// Disabled — do not reconnect.
    Shutdown,
    /// The relay superseded us (single-active). Reconnect soon.
    Goaway(String),
    /// Connection/handshake/keepalive failure. Backoff reconnect.
    Failed(String),
}

// ── One tunnel attempt ──────────────────────────────────────────────

async fn run_tunnel_once(
    identity: &RelayIdentity,
    url: &str,
    http_target: SocketAddr,
    shutdown: &mut watch::Receiver<bool>,
    status: &Arc<StatusCell>,
) -> TunnelEnd {
    // 1. Connect (WSS to the relay service domain).
    let ws = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url))
        .await
    {
        Err(_) => return TunnelEnd::Failed("connect timeout".into()),
        Ok(Err(e)) => return TunnelEnd::Failed(format!("connect: {e}")),
        Ok(Ok((ws, _response))) => ws,
    };

    let (driver, mut inbound_rx, driver_task) = spawn_driver(
        WsByteStream::tungstenite(ws),
        yamux::Config::default(),
        yamux::Mode::Client,
        64,
    );

    // 2. Handshake on a client-opened control stream (bounded).
    let (mut writer, reader, keepalive_s, session_id) =
        match tokio::time::timeout(CONNECT_TIMEOUT, handshake(&driver, identity)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                driver_task.abort();
                let _ = driver_task.await;
                return TunnelEnd::Failed(e);
            }
            Err(_) => {
                driver_task.abort();
                let _ = driver_task.await;
                return TunnelEnd::Failed("handshake timeout".into());
            }
        };
    {
        let mut s = status.write().unwrap();
        s.connected = true;
        s.session_id = Some(session_id.clone());
        s.keepalive_s = Some(keepalive_s);
        s.last_error = None;
        s.connected_at = Some(chrono::Utc::now());
    }
    tracing::info!(gw_id = %identity.gw_id, session_id = %session_id, "relay tunnel registered");

    // 3. Steady state.
    let (frames_tx, mut frames_rx) = mpsc::channel::<ControlFrame>(16);
    let reader_task = tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        // Unknown frame types are skipped, not fatal — a newer relay must not
        // be able to kill the session by using a feature we lack.
        while let Ok(frame) = read_control_frame_known(&mut reader).await {
            if frames_tx.send(frame).await.is_err() {
                break; // handler gone
            }
        }
        // Read error or stream reset — the caller observes the channel end.
    });

    let keepalive = Duration::from_secs(keepalive_s.max(1));
    let mut ping_tick = tokio::time::interval(keepalive);
    ping_tick.tick().await; // first tick completes immediately — skip it
    let mut last_pong = tokio::time::Instant::now();

    let end = loop {
        tokio::select! {
            inbound = inbound_rx.recv() => {
                let Some(stream) = inbound else {
                    break TunnelEnd::Failed("tunnel closed by relay".into());
                };
                tokio::spawn(serve_inbound(stream, http_target));
            }
            frame = frames_rx.recv() => {
                match frame {
                    Some(ControlFrame::Pong { .. }) => {
                        last_pong = tokio::time::Instant::now();
                    }
                    Some(ControlFrame::Goaway { reason }) => {
                        break TunnelEnd::Goaway(reason);
                    }
                    Some(other) => {
                        tracing::debug!(frame = ?other, "ignoring unexpected control frame");
                    }
                    None => {
                        break TunnelEnd::Failed("control stream closed".into());
                    }
                }
            }
            _ = ping_tick.tick() => {
                if last_pong.elapsed() > keepalive * 3 {
                    break TunnelEnd::Failed("keepalive timeout (no Pong)".into());
                }
                let ts_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                if write_control_frame(&mut writer, &ControlFrame::Ping { ts_ms })
                    .await
                    .is_err()
                {
                    break TunnelEnd::Failed("writing Ping".into());
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    // Graceful deregister: best effort, then grace so the
                    // frame reaches the wire before the abort (§5.4).
                    let _ = write_control_frame(&mut writer, &ControlFrame::Deregister).await;
                    tokio::time::sleep(TEARDOWN_GRACE).await;
                    break TunnelEnd::Shutdown;
                }
            }
        }
    };

    // 4. Teardown: abort the driver (drops the socket, closing every
    //    in-flight data stream) and stop the control reader.
    driver_task.abort();
    let _ = driver_task.await;
    reader_task.abort();
    let _ = reader_task.await;

    let mut s = status.write().unwrap();
    s.connected = false;
    s.session_id = None;
    s.connected_at = None;
    end
}

/// REGISTER → CHALLENGE → PROOF → REGISTERED on a client-opened stream
/// (§5.4). Returns `(writer half, reader half, keepalive_s, session_id)`
/// — the caller keeps the control stream split for Ping/Pong.
async fn handshake(
    driver: &acowork_core::relay::driver::YamuxDriverHandle,
    identity: &RelayIdentity,
) -> Result<
    (
        futures_util::io::WriteHalf<yamux::Stream>,
        futures_util::io::ReadHalf<yamux::Stream>,
        u64,
        String,
    ),
    String,
> {
    let stream = driver
        .open_stream(None)
        .await
        .map_err(|e| format!("opening control stream: {e}"))?;
    let mut control = BufReader::new(stream);

    // REGISTER
    let ts = chrono::Utc::now().timestamp().max(0) as u64;
    write_control_frame(
        control.get_mut(),
        &ControlFrame::Register {
            gw_id: identity.gw_id.clone(),
            pubkey: encode_pubkey(&identity.signing_key().verifying_key()),
            ts,
            proto: PROTO_VERSION,
            caps: Vec::new(),
        },
    )
    .await
    .map_err(|e| format!("writing REGISTER: {e}"))?;

    // CHALLENGE (or an immediate rejection — TOFU disabled, rate limit,
    // key mismatch from a previous life of this gw_id).
    let nonce = match read_control_frame_known(&mut control)
        .await
        .map_err(|e| format!("reading relay reply: {e}"))?
    {
        ControlFrame::Challenge { nonce } => nonce,
        ControlFrame::Rejected { reason } => {
            return Err(format!("relay rejected registration: {reason}"));
        }
        other => return Err(format!("expected CHALLENGE, got {other:?}")),
    };

    // PROOF — Ed25519 over the relay's single-use nonce.
    write_control_frame(
        control.get_mut(),
        &ControlFrame::Proof {
            sig: sign_nonce(identity.signing_key(), &nonce),
        },
    )
    .await
    .map_err(|e| format!("writing PROOF: {e}"))?;

    // REGISTERED
    match read_control_frame_known(&mut control)
        .await
        .map_err(|e| format!("reading REGISTERED: {e}"))?
    {
        ControlFrame::Registered {
            session_id,
            keepalive_s,
            proto,
            ..
        } => {
            // The relay pins a version at registration. A mismatch means the
            // two ends are not actually speaking the same protocol — surface
            // it rather than running a tunnel that misbehaves in ways neither
            // side can explain. (Both currently speak a single version, so
            // this is a guard, not a live path.)
            if proto != PROTO_VERSION {
                return Err(format!(
                    "relay registered tunnel at protocol version {proto}, \
                     this Gateway speaks {PROTO_VERSION} — versions are \
                     incompatible, upgrade the relay"
                ));
            }
            let (reader, writer) = control.into_inner().split();
            Ok((writer, reader, keepalive_s, session_id))
        }
        ControlFrame::Rejected { reason } => {
            Err(format!("relay rejected registration: {reason}"))
        }
        other => Err(format!("expected REGISTERED, got {other:?}")),
    }
}

/// Serve one relay-opened stream: read the opener's tag byte and forward
/// the raw bytes to the matching local listener (§5.2). HTTP streams go
/// to the remote-origin loopback listener, which enforces the remote ACL
/// (§7.2) — this function is a dumb pipe by design.
async fn serve_inbound(stream: yamux::Stream, http_target: SocketAddr) {
    let mut stream = stream;
    let mut tag = [0u8; 1];
    if stream.read_exact(&mut tag).await.is_err() {
        return; // closed before the tag arrived — nothing to serve
    }
    match tag[0] {
        STREAM_TAG_HTTP => {
            let upstream = match tokio::net::TcpStream::connect(http_target).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, target = %http_target, "relay stream: local listener unreachable");
                    return;
                }
            };
            let mut upstream = upstream.compat();
            // Same inactivity ceiling as the relay's device pipe (§5.2), so a
            // wedged stream is recycled on both ends of the tunnel. Byte
            // counts are logged because `in_bytes = 0` (request never
            // delivered) and `out_bytes = 0` (response never came back) are
            // different failures and looked identical before.
            let started = tokio::time::Instant::now();
            match copy_bidirectional(&mut stream, &mut upstream, PIPE_IDLE_TIMEOUT).await {
                Ok(stats) => tracing::info!(
                    dur_ms = started.elapsed().as_millis() as u64,
                    in_bytes = stats.a_to_b,
                    out_bytes = stats.b_to_a,
                    "relay stream pipe ended"
                ),
                Err(e) => tracing::info!(
                    dur_ms = started.elapsed().as_millis() as u64,
                    error = %e,
                    "relay stream pipe ended"
                ),
            }
            let _ = stream.close().await;
        }
        STREAM_TAG_MQTT => {
            // Reserved (§5.2): the v0.2 topology carries MQTT-over-WSS
            // through the HTTP listener's `/mqtt` route, so a raw MQTT
            // stream is unexpected — log and drop.
            tracing::warn!("relay stream: MQTT tag received but no raw MQTT listener exists; closing");
        }
        other => {
            tracing::warn!(tag = other, "relay stream: unknown tag; closing");
        }
    }
}
