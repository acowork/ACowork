//! M2 integration test (design doc 24 §8.2): the Gateway relay client
//! against a real in-process acowork-relay server.
//!
//! Covered:
//! - enable → identity minting → connect → REGISTER handshake →
//!   `status.connected = true` with session id
//! - device-domain HTTP request → relay byte pipe → tunnel → tagged
//!   stream → forwarded to the local "remote listener" → response
//!   piped back to the client
//! - disable → status flips, tunnel torn down (device domain then 502s)
//!
//! Topology note: the relay runs in plain (no-TLS) mode so the client
//! URL is `ws://127.0.0.1:{port}/tunnel`; routing peeks the Host header
//! (port-stripped), so `service_domain = "127.0.0.1"` matches. In
//! production the same code path carries `wss://relay.example.com/tunnel`
//! with SNI routing.

use std::time::Duration;

use acowork_gateway::relay::RelayClient;

#[tokio::test]
async fn relay_client_end_to_end() {
    // ── Relay server ─────────────────────────────────────────────────
    let relay_dir = tempfile::tempdir().unwrap().keep();
    let relay_config = acowork_relay::RelayConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        // Plain-mode Host routing: the client connects to 127.0.0.1 and
        // the Host header carries "127.0.0.1:<port>" (port stripped).
        service_domain: "127.0.0.1".into(),
        device_domain_suffix: "relay.test".into(),
        data_dir: relay_dir,
        // Short keepalive so the Ping/Pong path is exercised during the
        // test window.
        keepalive_s: 1,
        ..acowork_relay::RelayConfig::default()
    };
    let server = acowork_relay::entry::serve(relay_config).await.unwrap();
    let relay_addr = server.local_addr;

    // ── Local "remote listener" (stands in for :19877, M3) ──────────
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = echo.accept().await else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut head = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let n = match sock.read(&mut chunk).await {
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&chunk[..n]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let body = "hello via gateway relay client";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });

    // ── Relay client (the Gateway-side module under test) ────────────
    let identity_dir = tempfile::tempdir().unwrap().keep();
    let client = RelayClient::new(identity_dir.join("relay_identity.json"), listener_addr);

    // Disabled state is reported before enable.
    let status = client.status();
    assert!(!status.enabled && !status.connected, "{status:?}");

    client
        .enable(format!("ws://{relay_addr}/tunnel"))
        .await
        .expect("enable");

    // Wait for the handshake to complete.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = client.status();
        if status.connected {
            assert!(status.session_id.is_some(), "{status:?}");
            assert!(status.gw_id.is_some(), "{status:?}");
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

    // ── Data path: device domain → tunnel → local listener ───────────
    let mut tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let request = format!("GET /remote/api/agents HTTP/1.1\r\nHost: {gw_id}.relay.test\r\n\r\n");
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tcp.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("HTTP/1.1 200 OK"),
        "echo listener response missing: {text}"
    );
    assert!(
        text.contains("hello via gateway relay client"),
        "body not piped through the tunnel: {text}"
    );

    // ── Disable → torn down ──────────────────────────────────────────
    client.disable().await;
    let status = client.status();
    assert!(!status.enabled, "{status:?}");
    assert!(!status.connected, "{status:?}");

    // Give the teardown a moment, then the device domain must 502.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let request = format!("GET /x HTTP/1.1\r\nHost: {gw_id}.relay.test\r\n\r\n");
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tcp.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("502"), "expected DEVICE_OFFLINE 502: {text}");

    server.shutdown();
    server.stopped().await;
}

#[tokio::test]
async fn bad_urls_are_rejected_without_side_effects() {
    let dir = tempfile::tempdir().unwrap().keep();
    let client = RelayClient::new(dir.join("relay_identity.json"), "127.0.0.1:1".parse().unwrap());
    assert!(client.enable("https://relay.example.com".into()).await.is_err());
    assert!(client.enable("not-a-url".into()).await.is_err());
    // No identity file is minted for rejected enables.
    assert!(!dir.join("relay_identity.json").exists());
}
