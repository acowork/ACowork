//! M6 integration test (design doc 24 §10.1): the FULL relay data path
//! with every real component — no echo stand-ins.
//!
//! Topology under test (all in-process):
//!
//! ```text
//! reqwest / tungstenite client
//!   → acowork-relay (plain mode, Host routing)
//!     → device-domain byte pipe (yamux tunnel)
//!       → Gateway relay client stream demux
//!         → REAL remote HTTP listener (:TEST_REMOTE_HTTP)
//!           ├─ RemoteOrigin guard (§7.2 ACL)
//!           └─ /mqtt WS bridge → REAL strict MQTT listener (:TEST_REMOTE_MQTT)
//!                                → shared broker (Ed25519 token auth)
//! ```
//!
//! This closes the gap left by the M2 e2e (which stood an echo listener
//! in for :19877) and the M3 e2e (which exercised the listener directly,
//! without the relay in front): here the §7.2 ACL and the strict MQTT
//! auth are verified **through the tunnel**, which is the exact path a
//! remote Desktop takes in production.
//!
//! Assertions:
//! 1. HTTP: `GET /health` 200, debug/fs-browse paths 404 — the remote
//!    ACL holds end-to-end through the relay byte pipe.
//! 2. MQTT-over-WSS through the tunnel: a valid user access token
//!    CONNECTs (CONNACK rc=0); a wrong password is refused without a
//!    successful CONNACK.
//! 3. Teardown: after `disable`, the device domain answers 502
//!    (DEVICE_OFFLINE).

use std::sync::Arc;
use std::time::Duration;

use acowork_core::auth::now_unix;
use acowork_gateway::relay::RelayClient;
use acowork_gateway::relay::start_remote_http_listener;
use acowork_gateway::http::auth::HttpAuth;
use acowork_gateway::http::routes::AppState;
use acowork_gateway::mqtt::{start_broker_with_auth, RemoteMqttAuth, RemoteMqttListener};
use acowork_gateway::gateway::state::GatewayState;

/// Fixed test ports, offset from the M3 e2e ports (27974/27975/27977)
/// and far from the production defaults so parallel test runs and a
/// concurrently running Gateway never collide.
const TEST_MAIN_MQTT: u16 = 27995;
const TEST_REMOTE_MQTT: u16 = 27994;
const TEST_REMOTE_HTTP: u16 = 27997;

/// Mint the test key pair (same fixture idea as
/// `http::test_support`, which is `cfg(test)`-gated and thus invisible
/// to integration tests — the seed is a fixture, not a secret).
fn test_keys() -> (acowork_core::auth::TokenIssuer, acowork_core::auth::TokenVerifier) {
    let issuer = acowork_core::auth::TokenIssuer::new(ed25519_dalek::SigningKey::from_bytes(
        &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x00, 0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4,
        0xc3, 0xd2, 0xe1, 0xf0],
    ));
    let verifier = issuer.verifier();
    (issuer, verifier)
}

/// Minimal MQTT 3.1.1 CONNECT packet (protocol level 4, clean session,
/// username + password, keep-alive 60s). Same builder as the M3 e2e —
/// the strict listener's cross-check needs the real wire shape.
fn mqtt_connect(client_id: &str, username: &str, password: &str) -> Vec<u8> {
    fn lstr(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u16).to_be_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v
    }
    let mut vh = lstr("MQTT");
    vh.push(4); // protocol level
    vh.push(0xC2); // clean session | username | password
    vh.extend_from_slice(&60u16.to_be_bytes());
    let mut payload = lstr(client_id);
    payload.extend(lstr(username));
    payload.extend(lstr(password));
    let body = [vh, payload].concat();
    // MQTT remaining length: base-128 varint (tokens need 2+ bytes).
    let mut len = body.len();
    let mut pkt = vec![0x10u8];
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        pkt.push(byte);
        if len == 0 {
            break;
        }
    }
    pkt.extend_from_slice(&body);
    pkt
}

/// MQTT CONNACK return code, or None if the frame is not a CONNACK.
fn connack_return_code(frame: &[u8]) -> Option<u8> {
    if frame.len() >= 4 && frame[0] == 0x20 && frame[1] == 2 {
        Some(frame[3])
    } else {
        None
    }
}

/// Wait until a TCP port accepts connections (bounded).
async fn wait_listening(port: u16) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("port {port} never came up");
}

#[tokio::test]
async fn relay_full_path_e2e() {
    let (issuer, verifier) = test_keys();

    // ── Relay server (plain mode, Host-header routing) ───────────────
    let relay_dir = tempfile::tempdir().unwrap().keep();
    let relay_config = acowork_relay::RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        service_domain: "127.0.0.1".into(),
        device_domain_suffix: "relay.test".into(),
        data_dir: relay_dir,
        keepalive_s: 1,
        ..acowork_relay::RelayConfig::default()
    };
    let server = acowork_relay::entry::serve(relay_config).await.unwrap();
    let relay_port = server.local_addr.port();

    // ── Gateway-side: broker + strict remote MQTT listener ───────────
    let dir = tempfile::tempdir().unwrap();
    let mut gw = GatewayState::new(&dir.path().to_string_lossy());
    gw.user_verifier = Some(Arc::new(verifier));
    let gateway_state = Arc::new(tokio::sync::RwLock::new(gw));

    let _broker = start_broker_with_auth(
        "127.0.0.1",
        TEST_MAIN_MQTT,
        None,
        Some(RemoteMqttListener {
            host: "127.0.0.1".to_string(),
            port: TEST_REMOTE_MQTT,
            auth: RemoteMqttAuth {
                gateway_state: gateway_state.clone(),
            },
        }),
    )
    .expect("broker with strict listener starts");
    // The remote MQTT server binds asynchronously inside the broker's
    // OS thread — the WS bridge dials it per-request, so it must be up
    // before the first MQTT-over-WS assertion.
    wait_listening(TEST_REMOTE_MQTT).await;

    // ── Gateway-side: REAL remote HTTP listener (guard + WS bridge) ──
    let state = AppState::new(gateway_state.clone(), Arc::new(HttpAuth::new(false)));
    let http_server = tokio::spawn(start_remote_http_listener(
        state,
        TEST_REMOTE_HTTP,
        TEST_REMOTE_MQTT,
    ));
    wait_listening(TEST_REMOTE_HTTP).await;

    // ── Gateway relay client: tunnel to the relay ────────────────────
    let identity_dir = tempfile::tempdir().unwrap().keep();
    let client = RelayClient::new(
        identity_dir.join("relay_identity.json"),
        format!("127.0.0.1:{TEST_REMOTE_HTTP}").parse().unwrap(),
    );
    client
        .enable(format!("ws://127.0.0.1:{relay_port}/tunnel"))
        .await
        .expect("enable");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = client.status();
        if status.connected {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tunnel never connected: {:?}",
            status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let gw_id = client.status().gw_id.clone().unwrap();
    // Plain-mode routing peeks the Host header; the device domain is
    // `<gw-id>.relay.test` (what a hosts-file entry would map at the
    // relay's address in production).
    let device_host = format!("{gw_id}.relay.test");

    // ── 1. HTTP through the tunnel: ACL holds end-to-end ─────────────
    let http = reqwest::Client::new();
    let relay_base = format!("http://127.0.0.1:{relay_port}");

    let health = http
        .get(format!("{relay_base}/health"))
        .header("Host", &device_host)
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200, "device domain must reach /health through the tunnel");

    let debug = http
        .post(format!("{relay_base}/api/debug/mqtt/start"))
        .header("Host", &device_host)
        .send()
        .await
        .unwrap();
    assert_eq!(debug.status(), 404, "debug endpoints must stay hidden through the tunnel");

    let fs = http
        .get(format!("{relay_base}/api/fs/browse"))
        .header("Host", &device_host)
        .send()
        .await
        .unwrap();
    assert_eq!(fs.status(), 404, "fs browse must stay hidden through the tunnel");

    // ── 2. MQTT over WSS through the tunnel ──────────────────────────
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    use tokio_tungstenite::tungstenite::http::Request;

    /// WS-upgrade request at the relay's address but carrying the DEVICE
    /// domain in the Host header (production: DNS maps the device domain
    /// to the relay; the SNI/Host router then picks the tunnel).
    async fn ws_to_relay(relay_port: u16, device_host: &str) -> (
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
    ) {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", relay_port))
            .await
            .expect("connect to relay");
        let request = Request::builder()
            .uri(format!("ws://127.0.0.1:{relay_port}/mqtt"))
            .header("Host", device_host)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .header("Sec-WebSocket-Protocol", "mqtt")
            .body(())
            .unwrap();
        tokio_tungstenite::client_async(request, stream)
            .await
            .expect("WS upgrade through the tunnel")
    }

    // Valid credential: CONNECT → CONNACK rc=0.
    let token = issuer.sign_access("alice", "user", now_unix());
    let (mut ws, _) = ws_to_relay(relay_port, &device_host).await;
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        mqtt_connect("user:alice:desktop:remote-1", "alice", &token).into(),
    ))
    .await
    .unwrap();
    let connack = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("CONNACK within 5s")
        .expect("stream open")
        .expect("ws read ok");
    match connack {
        tokio_tungstenite::tungstenite::Message::Binary(data) => {
            assert_eq!(
                connack_return_code(&data),
                Some(0),
                "valid access token must CONNECT through the tunnel (got {data:?})"
            );
        }
        other => panic!("expected binary CONNACK, got {other:?}"),
    }

    // Wrong password: the strict listener refuses and drops the
    // connection (rumqttd InvalidAuth — no CONNACK). Anything but a
    // successful CONNACK is acceptable.
    let (mut ws, _) = ws_to_relay(relay_port, &device_host).await;
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        mqtt_connect("user:alice:desktop:remote-1", "alice", "wrong-password").into(),
    ))
    .await
    .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("reply within 5s");
    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) = reply {
        assert_ne!(
            connack_return_code(&data),
            Some(0),
            "bad credentials must not CONNECT through the tunnel (got {data:?})"
        );
    }

    // ── 3. Teardown: disable → device domain 502 (DEVICE_OFFLINE) ────
    client.disable().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let resp = http
        .get(format!("{relay_base}/x"))
        .header("Host", &device_host)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "device domain must report DEVICE_OFFLINE after disable");

    http_server.abort();
    server.shutdown();
    server.stopped().await;
}
