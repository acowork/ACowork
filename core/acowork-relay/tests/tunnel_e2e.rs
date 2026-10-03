//! M1 integration tests (design doc 24 §5.2–§5.4): a fake Gateway tunnel
//! client speaks the real handshake and byte-pipe protocol against a live
//! in-process relay (plain mode — Host-header routing).
//!
//! Covered paths:
//! - service-domain routing (`/health`)
//! - device-domain miss → handcrafted 502 DEVICE_OFFLINE
//! - full data path: REGISTER handshake → relay-opened tagged stream →
//!   HTTP request piped through → response piped back
//! - control-channel liveness (Ping → Pong)
//! - single-active eviction (second registration → GOAWAY to the first)
//! - TOFU key pinning (same gw-id, different key → Rejected)
//! - TOFU disabled (`require_registration` → unknown device Rejected)

use std::net::SocketAddr;
use std::time::Duration;

use acowork_core::relay::driver::{YamuxDriverHandle, spawn_driver};
use acowork_core::relay::proto::{
    encode_pubkey, generate_device_seed, sign_nonce, ControlFrame,
};
use acowork_core::relay::ws_stream::WsByteStream;
use acowork_core::relay::STREAM_TAG_HTTP;
use acowork_relay::config::RelayConfig;
use acowork_relay::entry::{self, RelayServer};
use ed25519_dalek::SigningKey;
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(10);

// ── Harness ─────────────────────────────────────────────────────────

async fn spawn_relay(require_registration: bool) -> (RelayServer, SocketAddr) {
    let dir = tempfile::tempdir().unwrap().keep();
    let config = RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        service_domain: "relay.test".into(),
        device_domain_suffix: "relay.test".into(),
        data_dir: dir,
        require_registration,
        ..RelayConfig::default()
    };
    let server = entry::serve(config).await.expect("relay binds");
    let addr = server.local_addr;
    (server, addr)
}

/// A raw one-shot HTTP request: connects, sends, reads to EOF.
/// `Connection: close` keeps the read bounded on keep-alive servers.
async fn http_get(addr: SocketAddr, host: &str, path: &str) -> String {
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tcp.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

// ── Fake Gateway ────────────────────────────────────────────────────

/// Client side of the tunnel: WSS + yamux (Client mode), driven by the
/// shared driver (`spawn_driver`) — the same code the Gateway's
/// relay-client module will run.
struct FakeGateway {
    gw_id: String,
    key: SigningKey,
    driver: YamuxDriverHandle,
    inbound_rx: mpsc::Receiver<yamux::Stream>,
}

async fn connect_gateway(addr: SocketAddr, gw_id: String, key: SigningKey) -> FakeGateway {
    let tcp = TcpStream::connect(addr).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    // The URI authority becomes the Host header, which is what plain-mode
    // routing peeks; the actual TCP peer is `addr`.
    let request = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        "ws://relay.test/tunnel",
    )
    .unwrap();
    let (ws, _response) = tokio_tungstenite::client_async(request, tcp)
        .await
        .unwrap();
    let (driver, inbound_rx, _driver_task) = spawn_driver(
        WsByteStream::tungstenite(ws),
        yamux::Config::default(),
        yamux::Mode::Client,
        64,
    );
    FakeGateway {
        gw_id,
        key,
        driver,
        inbound_rx,
    }
}

impl FakeGateway {
    /// Open a yamux stream through the driver (client → relay).
    async fn open_stream(&self) -> anyhow::Result<yamux::Stream> {
        self.driver.open_stream(None).await
    }

    /// REGISTER → CHALLENGE → PROOF → REGISTERED. Returns the session id
    /// and the still-open control stream.
    async fn register(&self) -> anyhow::Result<(String, yamux::Stream)> {
        let mut control = self.open_stream().await?;
        write_frame(
            &mut control,
            &ControlFrame::Register {
                gw_id: self.gw_id.clone(),
                pubkey: encode_pubkey(&self.key.verifying_key()),
                ts: 0,
                proto: acowork_core::relay::proto::PROTO_VERSION,
                caps: Vec::new(),
            },
        )
        .await?;
        let nonce = match read_frame(&mut control).await? {
            ControlFrame::Challenge { nonce } => nonce,
            ControlFrame::Rejected { reason } => anyhow::bail!("rejected: {reason}"),
            other => anyhow::bail!("expected CHALLENGE, got {other:?}"),
        };
        write_frame(
            &mut control,
            &ControlFrame::Proof {
                sig: sign_nonce(&self.key, &nonce),
            },
        )
        .await?;
        match read_frame(&mut control).await? {
            ControlFrame::Registered { session_id, .. } => Ok((session_id, control)),
            ControlFrame::Rejected { reason } => anyhow::bail!("rejected: {reason}"),
            other => anyhow::bail!("expected REGISTERED, got {other:?}"),
        }
    }
}

/// Serve relay-opened streams the way the Gateway's remote listener will:
/// read the tag byte, then the request head, answer with a marker body.
async fn echo_inbound(mut inbound_rx: mpsc::Receiver<yamux::Stream>, marker: String) {
    while let Some(mut stream) = inbound_rx.recv().await {
        let marker = marker.clone();
        tokio::spawn(async move {
            let mut tag = [0u8; 1];
            stream.read_exact(&mut tag).await?;
            assert_eq!(tag[0], STREAM_TAG_HTTP, "relay must tag HTTP streams");

            // Read the request head (up to the blank line).
            let mut head = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                if stream.read(&mut byte).await? == 0 {
                    break;
                }
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }

            let body = format!("hello from {marker}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.close().await?;
            anyhow::Ok(())
        });
    }
}

// ── ndjson control-frame IO (shared implementation) ────────────────

async fn read_frame<S>(stream: &mut S) -> anyhow::Result<ControlFrame>
where
    S: futures_util::io::AsyncRead + Unpin,
{
    acowork_core::relay::proto::read_control_frame_known(stream)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

async fn write_frame<W>(stream: &mut W, frame: &ControlFrame) -> anyhow::Result<()>
where
    W: futures_util::io::AsyncWrite + Unpin,
{
    acowork_core::relay::proto::write_control_frame(stream, frame)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn service_domain_serves_health() {
    let (server, addr) = spawn_relay(false).await;
    let response = http_get(addr, "relay.test", "/health").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("ok"), "{response}");
    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn offline_device_returns_502() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();
    let response = http_get(addr, &format!("{gw_id}.relay.test"), "/api/agents").await;
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
    assert!(response.contains("DEVICE_OFFLINE"), "{response}");
    server.shutdown();
    server.stopped().await;
}

// ── Protocol-version negotiation ───────────────────────────────────
//
// The control protocol is a published contract: the open-source relay and
// out-of-tree implementations must interoperate across revisions. These
// tests pin the two properties that make that possible — old peers keep
// working, and mismatched versions fail loudly instead of silently.

/// A Gateway predating the `proto` field must still be able to register.
/// This is the compatibility guarantee that lets the protocol ship frozen.
#[tokio::test]
async fn a_legacy_gateway_without_proto_still_registers() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();
    let key = SigningKey::from_bytes(&generate_device_seed());
    let gateway = connect_gateway(addr, gw_id.clone(), key).await;

    // Hand-written REGISTER: exactly the pre-versioning wire format, with no
    // `proto` and no `caps` key at all.
    let mut control = gateway.open_stream().await.unwrap();
    let line = format!(
        r#"{{"type":"register","gw_id":"{gw_id}","pubkey":"{}","ts":0}}"#,
        encode_pubkey(&gateway.key.verifying_key())
    );
    control.write_all(line.as_bytes()).await.unwrap();
    control.write_all(b"\n").await.unwrap();
    control.flush().await.unwrap();

    let nonce = match read_frame(&mut control).await.unwrap() {
        ControlFrame::Challenge { nonce } => nonce,
        other => panic!("legacy Gateway must reach CHALLENGE, got {other:?}"),
    };
    write_frame(
        &mut control,
        &ControlFrame::Proof {
            sig: sign_nonce(&gateway.key, &nonce),
        },
    )
    .await
    .unwrap();
    match read_frame(&mut control).await.unwrap() {
        // The relay pins the version it actually spoke.
        ControlFrame::Registered { proto, .. } => assert_eq!(proto, 1),
        other => panic!("legacy Gateway must reach REGISTERED, got {other:?}"),
    }

    server.shutdown();
    server.stopped().await;
}

/// A Gateway announcing a revision this relay does not implement must be
/// refused *before* it can enroll — not silently half-connected.
#[tokio::test]
async fn a_future_proto_version_is_rejected() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();
    let key = SigningKey::from_bytes(&generate_device_seed());
    let gateway = connect_gateway(addr, gw_id.clone(), key).await;

    let mut control = gateway.open_stream().await.unwrap();
    write_frame(
        &mut control,
        &ControlFrame::Register {
            gw_id: gw_id.clone(),
            pubkey: encode_pubkey(&gateway.key.verifying_key()),
            ts: 0,
            proto: acowork_core::relay::proto::PROTO_VERSION + 1,
            caps: vec!["from.the.future".into()],
        },
    )
    .await
    .unwrap();

    match read_frame(&mut control).await.unwrap() {
        ControlFrame::Rejected { reason } => {
            assert!(
                reason.contains("protocol version"),
                "rejection should name the cause, got: {reason}"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    // And the device must not have been enrolled by the failed attempt.
    assert!(
        !http_get(addr, &format!("{gw_id}.relay.test"), "/api/agents")
            .await
            .contains("HTTP/1.1 200"),
        "a version-mismatched Gateway must not end up routable"
    );

    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn tunnel_forwards_http_and_keeps_liveness() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();
    let key = SigningKey::from_bytes(&generate_device_seed());

    let gateway = connect_gateway(addr, gw_id.clone(), key).await;
    let (_session, mut control) = gateway
        .register()
        .await
        .expect("REGISTER handshake completes");
    tokio::spawn(echo_inbound(gateway.inbound_rx, format!("gateway-{gw_id}")));

    // Control-channel liveness: Ping → Pong with the ts echoed.
    write_frame(&mut control, &ControlFrame::Ping { ts_ms: 12345 })
        .await
        .unwrap();
    match tokio::time::timeout(TIMEOUT, read_frame(&mut control))
        .await
        .expect("Pong arrives")
        .unwrap()
    {
        ControlFrame::Pong { ts_ms } => assert_eq!(ts_ms, 12345),
        other => panic!("expected Pong, got {other:?}"),
    }

    // Data path: device-domain request → tagged yamux stream → echo.
    let response = http_get(addr, &format!("{gw_id}.relay.test"), "/remote/api/agents").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains(&format!("hello from gateway-{gw_id}")),
        "{response}"
    );

    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn second_registration_evicts_first() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();
    let key = SigningKey::from_bytes(&generate_device_seed());

    let first = connect_gateway(addr, gw_id.clone(), key.clone()).await;
    let (_session, mut control) = first.register().await.expect("first registration");

    let second = connect_gateway(addr, gw_id.clone(), key).await;
    second.register().await.expect("second registration");
    tokio::spawn(echo_inbound(second.inbound_rx, "second".into()));

    // The first tunnel must observe its eviction via GOAWAY (not just die).
    let frame = tokio::time::timeout(TIMEOUT, read_frame(&mut control))
        .await
        .expect("GOAWAY arrives before the first tunnel is killed")
        .expect("control stream still readable");
    match frame {
        ControlFrame::Goaway { reason } => {
            assert!(reason.contains("superseded"), "{reason}");
        }
        other => panic!("expected GOAWAY, got {other:?}"),
    }

    // The new tunnel now owns the device domain (requests reach the second
    // gateway's echo, not the dead first one).
    let response = http_get(addr, &format!("{gw_id}.relay.test"), "/x").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("hello from second"), "{response}");

    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn pinned_key_mismatch_is_rejected() {
    let (server, addr) = spawn_relay(false).await;
    let gw_id = uuid::Uuid::new_v4().to_string();

    // TOFU: first key enrolls.
    let first = connect_gateway(addr, gw_id.clone(), SigningKey::from_bytes(&generate_device_seed())).await;
    first.register().await.expect("TOFU enrollment");

    // A different key for the same gw-id is refused (key pinning).
    let second = connect_gateway(addr, gw_id, SigningKey::from_bytes(&generate_device_seed())).await;
    let error = second.register().await.expect_err("pinned-key mismatch must be rejected");
    assert!(error.to_string().contains("rejected"), "{error}");
    assert!(error.to_string().contains("pinned"), "{error}");

    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn tofu_disabled_rejects_unknown_device() {
    let (server, addr) = spawn_relay(true).await;
    let gateway = connect_gateway(
        addr,
        uuid::Uuid::new_v4().to_string(),
        SigningKey::from_bytes(&generate_device_seed()),
    )
    .await;
    let error = gateway
        .register()
        .await
        .expect_err("unknown device must be rejected when TOFU is off");
    assert!(error.to_string().contains("TOFU disabled"), "{error}");

    server.shutdown();
    server.stopped().await;
}
