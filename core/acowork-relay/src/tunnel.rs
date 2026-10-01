//! Per-tunnel lifecycle (design doc 24 §5.4): handshake, control loop.
//!
//! One tunnel = one Gateway's outbound WSS connection. Cooperators:
//!
//! - the shared yamux driver ([`acowork_core::relay::driver`], spawned in
//!   `Mode::Server`) — delivers the Gateway-opened control stream as the
//!   first inbound and opens tagged data streams toward the Gateway.
//! - handshake — inline in [`run_tunnel`]: REGISTER → CHALLENGE → PROOF
//!   → REGISTERED on the control stream, then registry insertion.
//! - [`run_control`] — keeps the control stream after the handshake:
//!   Ping→Pong liveness, RotateKey, Deregister, GOAWAY on eviction.
//!
//! Teardown rule: when the control task ends (for ANY reason — idle
//! timeout, GOAWAY, Deregister, stream close), the driver task is
//! aborted, which drops the socket and closes the tunnel. This also
//! covers eviction (`goaway_and_kill`) and keeps `run_tunnel` from
//! waiting on a driver whose peer simply stopped talking.

use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;

use acowork_core::relay::driver::spawn_driver;
use acowork_core::relay::proto::{
    self, ControlFrame, generate_nonce, is_valid_gw_id, verify_key_rotation, verify_nonce_sig,
};
use acowork_core::relay::ws_stream::WsByteStream;
use acowork_core::relay::TEARDOWN_GRACE;

use crate::config::RelayConfig;
use crate::device_store::DeviceStore;
use crate::registry::{TunnelHandle, TunnelRegistry};

/// How long the whole handshake may take after the WS connection opens.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Shared relay state a tunnel needs.
#[derive(Clone)]
pub struct TunnelContext {
    pub registry: Arc<TunnelRegistry>,
    pub devices: Arc<DeviceStore>,
    pub config: Arc<RelayConfig>,
}

impl TunnelContext {
    pub fn new(
        registry: Arc<TunnelRegistry>,
        devices: Arc<DeviceStore>,
        config: Arc<RelayConfig>,
    ) -> Self {
        Self {
            registry,
            devices,
            config,
        }
    }
}

/// Thin anyhow wrappers over the shared ndjson frame IO (§5.4).
async fn read_frame<S>(control: &mut S) -> anyhow::Result<ControlFrame>
where
    S: futures_util::io::AsyncRead + Unpin,
{
    proto::read_control_frame(control).await.map_err(|e| anyhow!(e))
}

async fn write_frame<W>(control: &mut W, frame: &ControlFrame) -> anyhow::Result<()>
where
    W: futures_util::io::AsyncWrite + Unpin,
{
    proto::write_control_frame(control, frame)
        .await
        .map_err(|e| anyhow!(e))
}

/// Best-effort polite rejection on the handshake control stream: tell the
/// Gateway why, then close (the Gateway's relay-client surfaces the reason
/// in its state machine).
async fn reject_handshake(mut control: BufReader<yamux::Stream>, reason: &str) {
    let _ = write_frame(
        control.get_mut(),
        &ControlFrame::Rejected { reason: reason.into() },
    )
    .await;
    let _ = control.into_inner().close().await;
}

/// Run one tunnel to completion: handshake, register, control loop,
/// teardown, unregister.
pub async fn run_tunnel(io: WsByteStream, ctx: TunnelContext) {
    let (driver, mut inbound_rx, driver_task) =
        spawn_driver(io, yamux::Config::default(), yamux::Mode::Server, 8);

    // ── Handshake on the first inbound (Gateway-opened) stream ───────
    let outcome = tokio::time::timeout(HANDSHAKE_TIMEOUT, inbound_rx.recv()).await;
    let (gw_id, session_id, mut control) = match outcome {
        // Timeout with no control stream in sight.
        Err(_) => {
            driver_task.abort();
            let _ = driver_task.await;
            return;
        }
        // Driver died before a stream arrived.
        Ok(None) => {
            let _ = driver_task.await;
            return;
        }
        Ok(Some(stream)) => match handshake(BufReader::new(stream), &ctx).await {
            Ok(v) => v,
            Err(e) => {
                tracing::info!(error = %e, "tunnel handshake rejected");
                // Grace: a Rejected frame may have been queued on the
                // control stream — give the driver a poll cycle to put
                // it on the wire before the teardown (TEARDOWN_GRACE).
                tokio::time::sleep(TEARDOWN_GRACE).await;
                driver_task.abort();
                let _ = driver_task.await;
                return;
            }
        },
    };

    // Registered: build the handle and evict any predecessor.
    let (goaway_tx, goaway_rx) = mpsc::channel::<crate::registry::ControlOut>(4);
    let handle = TunnelHandle::new(
        session_id.clone(),
        driver.clone(),
        goaway_tx,
        Arc::new(tokio::sync::Semaphore::new(ctx.config.max_conns_per_gateway)),
        driver_task.abort_handle(),
    );
    let evicted = match ctx.registry.register(&gw_id, handle.clone()) {
        Ok(old) => old,
        Err(e) => {
            // Capacity: reject politely and tear down (grace so the
            // Rejected frame reaches the wire — TEARDOWN_GRACE).
            let _ = write_frame(
                control.get_mut(),
                &ControlFrame::Rejected { reason: e },
            )
            .await;
            tokio::time::sleep(TEARDOWN_GRACE).await;
            handle.kill();
            let _ = driver_task.await;
            return;
        }
    };
    if let Some(old) = evicted {
        // GOAWAY then kill — the old tunnel observes its eviction.
        old.goaway_and_kill("superseded by a newer registration").await;
    }

    tracing::info!(gw_id = %gw_id, session_id = %session_id, "tunnel registered");

    // Protocol guard: after the control stream, the Gateway never opens
    // more streams toward the relay. Drain (and log) any that appear so
    // a violating peer cannot stall the driver's inbound channel.
    tokio::spawn(async move {
        while inbound_rx.recv().await.is_some() {
            tracing::warn!("unexpected extra inbound stream from gateway; resetting");
        }
    });

    // ── Steady state ────────────────────────────────────────────────
    // The control channel (goaway_rx) is consumed by run_control; the
    // driver ends via abort when the control task finishes (any reason).
    let control_task = tokio::spawn(run_control(
        control,
        gw_id.clone(),
        session_id.clone(),
        ctx.clone(),
        goaway_rx,
    ));

    let control_res = control_task.await;
    if let Err(e) = &control_res {
        tracing::debug!(error = %e, "tunnel control task ended with error");
    }

    // Control over → tunnel over. Grace first: farewell frames (GOAWAY,
    // Rejected) may still be queued in the driver — give them a poll
    // cycle to reach the wire before the abort drops the socket.
    tokio::time::sleep(TEARDOWN_GRACE).await;
    driver_task.abort();
    let _ = driver_task.await;

    ctx.registry.remove_session(&gw_id, &session_id);
    tracing::info!(gw_id = %gw_id, session_id = %session_id, "tunnel closed");
}

/// The REGISTER → CHALLENGE → PROOF → REGISTERED exchange on the first
/// inbound stream. Returns `(gw_id, session_id, control stream)` on
/// success; `Err` carries the reject reason (a `Rejected` frame has
/// already been sent to the Gateway where possible).
async fn handshake(
    mut control: BufReader<yamux::Stream>,
    ctx: &TunnelContext,
) -> anyhow::Result<(String, String, BufReader<yamux::Stream>)> {
    // REGISTER
    let (gw_id, pubkey) = match read_frame(&mut control).await? {
        ControlFrame::Register { gw_id, pubkey, .. } => (gw_id, pubkey),
        other => {
            let reason = format!("expected REGISTER, got {other:?}");
            reject_handshake(control, &reason).await;
            anyhow::bail!(reason);
        }
    };

    macro_rules! reject {
        ($reason:expr) => {{
            let reason: String = $reason;
            reject_handshake(control, &reason).await;
            anyhow::bail!(reason);
        }};
    }

    if !is_valid_gw_id(&gw_id) {
        reject!(format!("invalid gw_id '{gw_id}' (must be UUID v4)"));
    }
    if !ctx.registry.check_register_rate(&gw_id, 5) {
        reject!(format!("register rate limit exceeded for {gw_id}"));
    }
    if ctx.config.require_registration && !ctx.devices.contains(&gw_id) {
        reject!(format!("unknown device {gw_id} (TOFU disabled on this relay)"));
    }
    // TOFU enroll or pin-check (rejects on key mismatch).
    let verifying = match ctx.devices.enroll_or_verify(&gw_id, &pubkey) {
        Ok(key) => key,
        Err(e) => reject!(format!("device enrollment: {e}")),
    };

    // CHALLENGE
    let nonce = generate_nonce();
    if let Err(e) =
        write_frame(control.get_mut(), &ControlFrame::Challenge { nonce: nonce.clone() }).await
    {
        anyhow::bail!("writing CHALLENGE: {e}");
    }

    // PROOF
    match read_frame(&mut control).await? {
        ControlFrame::Proof { sig } => {
            if !verify_nonce_sig(&verifying, &nonce, &sig) {
                reject!(format!("invalid proof signature for {gw_id}"));
            }
        }
        other => {
            let reason = format!("expected PROOF, got {other:?}");
            reject_handshake(control, &reason).await;
            anyhow::bail!(reason);
        }
    }

    // REGISTERED
    let session_id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = write_frame(
        control.get_mut(),
        &ControlFrame::Registered {
            session_id: session_id.clone(),
            keepalive_s: ctx.config.keepalive_s,
        },
    )
    .await
    {
        anyhow::bail!("writing REGISTERED: {e}");
    }

    Ok((gw_id, session_id, control))
}

/// Steady-state control stream loop: liveness, key rotation, deregistration,
/// and eviction (GOAWAY).
///
/// The control stream is split into read/write halves: a dedicated reader
/// task yields parsed frames, this loop owns the writer and reacts. Any
/// control traffic re-arms the idle deadline (tunnels whose gateway stopped
/// responding get reaped).
async fn run_control(
    control: BufReader<yamux::Stream>,
    gw_id: String,
    session_id: String,
    ctx: TunnelContext,
    mut goaway_rx: mpsc::Receiver<crate::registry::ControlOut>,
) -> anyhow::Result<()> {
    let (read_half, mut writer) = control.into_inner().split();

    // Frame reader task → channel.
    let (frames_tx, mut frames_rx) = mpsc::channel::<ControlFrame>(16);
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        loop {
            match read_frame(&mut reader).await {
                Ok(frame) => {
                    if frames_tx.send(frame).await.is_err() {
                        break; // handler gone
                    }
                }
                Err(e) => {
                    // Read error or stream reset — the connection is done.
                    tracing::debug!(error = %e, "control reader ended");
                    break;
                }
            }
        }
    });

    let keepalive = ctx.config.keepalive_s.max(1);
    let mut idle_deadline = tokio::time::Instant::now() + Duration::from_secs(keepalive * 3);

    loop {
        let timeout = tokio::time::sleep_until(idle_deadline);
        tokio::select! {
            frame = frames_rx.recv() => {
                let Some(frame) = frame else {
                    anyhow::bail!("control stream closed");
                };
                idle_deadline = tokio::time::Instant::now() + Duration::from_secs(keepalive * 3);
                match frame {
                    ControlFrame::Ping { ts_ms } => {
                        write_frame(&mut writer, &ControlFrame::Pong { ts_ms }).await?;
                    }
                    ControlFrame::RotateKey { new_pubkey, sig } => {
                        // Signature checked against the CURRENTLY pinned key.
                        match ctx.devices.get_pubkey(&gw_id) {
                            Ok(Some(pinned)) if verify_key_rotation(&pinned, &new_pubkey, &sig) => {
                                match ctx.devices.rotate_key(&gw_id, &new_pubkey) {
                                    Ok(()) => tracing::info!(gw_id = %gw_id, "device key rotated"),
                                    Err(e) => tracing::warn!(gw_id = %gw_id, error = %e, "key rotation persist failed"),
                                }
                            }
                            _ => {
                                tracing::warn!(gw_id = %gw_id, "rejected key rotation (bad signature or missing pin)");
                            }
                        }
                    }
                    ControlFrame::Deregister => {
                        tracing::info!(gw_id = %gw_id, session_id = %session_id, "graceful deregister");
                        let _ = writer.close().await;
                        return Ok(());
                    }
                    other => {
                        tracing::debug!(frame = ?other, "ignoring unexpected control frame");
                    }
                }
            }
            out = goaway_rx.recv() => {
                if let Some(crate::registry::ControlOut::Goaway { reason }) = out {
                    let _ = write_frame(&mut writer, &ControlFrame::Goaway { reason }).await;
                    return Ok(()); // the evictor kills the driver after the grace
                }
            }
            _ = timeout => {
                tracing::info!(gw_id = %gw_id, "tunnel idle timeout (no control traffic)");
                anyhow::bail!("control idle timeout");
            }
        }
    }
}
