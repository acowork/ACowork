//! Relay entry point (design doc 24 §5.3): one TCP listener, SNI-routed.
//!
//! ```text
//! TCP :443 accept
//!   ├─ TLS ClientHello SNI == service domain  → axum (control plane):
//!   │     GET /tunnel (WS upgrade → run_tunnel), /health, /api/admin/*
//!   └─ SNI == <gw-id>.<suffix>                 → raw byte pipe:
//!         tunnel registry hit → yamux stream (tag 0x01) ↔ client TLS stream
//!         miss → handcrafted HTTP 502 DEVICE_OFFLINE
//! ```
//!
//! No HTTP parsing on the device-domain path (v0.2: the relay is a pure
//! SNI-routed byte pipe). TLS-off mode (dev/tests) routes by the HTTP Host
//! header instead, using `TcpStream::peek` so nothing is consumed.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
use parking_lot::Mutex;
use tokio::io::{AsyncRead as TokioAsyncRead, AsyncWrite as TokioAsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use acowork_core::relay::driver::copy_bidirectional;
use acowork_core::relay::proto::DEVICE_OFFLINE_BODY;
use acowork_core::relay::ws_stream::WsByteStream;
use acowork_core::relay::{PIPE_IDLE_TIMEOUT, STREAM_TAG_HTTP, TEARDOWN_GRACE};

use crate::config::RelayConfig;
use crate::device_store::DeviceStore;
use crate::registry::TunnelRegistry;
use crate::tunnel::{TunnelContext, run_tunnel};

/// Handle to the running relay.
pub struct RelayServer {
    /// The bound listener address (useful when port 0 was requested).
    pub local_addr: SocketAddr,
    shutdown: tokio::sync::watch::Sender<bool>,
    done: tokio::task::JoinHandle<()>,
}

impl RelayServer {
    /// Ask the server to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Wait for the server tasks to finish (after shutdown was requested).
    pub async fn stopped(self) {
        let _ = self.done.await;
    }
}

/// Per-IP concurrent connection limiter (§5.5 anti-scan).
struct IpLimiter {
    max_per_ip: usize,
    counts: Mutex<HashMap<std::net::IpAddr, usize>>,
}

impl IpLimiter {
    fn new(max_per_ip: usize) -> Self {
        Self {
            max_per_ip,
            counts: Mutex::new(HashMap::new()),
        }
    }

    fn try_acquire(&self, ip: std::net::IpAddr) -> Option<IpGuard<'_>> {
        let mut counts = self.counts.lock();
        let entry = counts.entry(ip).or_insert(0);
        if *entry >= self.max_per_ip {
            return None;
        }
        *entry += 1;
        Some(IpGuard { limiter: self, ip })
    }
}

struct IpGuard<'a> {
    limiter: &'a IpLimiter,
    ip: std::net::IpAddr,
}

impl Drop for IpGuard<'_> {
    fn drop(&mut self) {
        let mut counts = self.limiter.counts.lock();
        if let Some(count) = counts.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

/// Serve the relay. Returns once the listener is bound; the server keeps
/// running on background tasks until [`RelayServer::shutdown`].
pub async fn serve(config: RelayConfig) -> Result<RelayServer> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding relay listener on {}", config.listen))?;
    let local_addr = listener.local_addr()?;

    let ctx = TunnelContext::new(
        Arc::new(TunnelRegistry::new(config.max_tunnels)),
        Arc::new(DeviceStore::load(&config.data_dir)?),
        Arc::new(config.clone()),
    );

    // TLS (when configured). ALPN intentionally empty: not offering h2
    // forces HTTP/1.1 on the data plane, which a raw byte pipe can carry.
    let tls = match (config.tls_enabled(), &config.tls_cert, &config.tls_key) {
        (true, Some(cert), Some(key)) => Some(build_tls_config(cert, key)?),
        (false, None, None) => None,
        _ => anyhow::bail!("tls_cert and tls_key must be provided together"),
    };

    // Control-plane plumbing: accept loop pushes service-domain
    // connections into the channel; axum::serve drains it.
    let (service_tx, service_rx) = tokio::sync::mpsc::channel::<std::io::Result<ServiceIo>>(64);
    let service_listener = ServiceConns { rx: service_rx };
    let router = build_router(ctx.clone());
    let http = tokio::spawn(async move {
        let app = axum::serve(service_listener, router.into_make_service());
        if let Err(e) = app.await {
            tracing::error!(error = %e, "relay control-plane http server ended");
        }
    });

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let limiter = Arc::new(IpLimiter::new(64));

    let done = tokio::spawn(async move {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (tcp, peer) = match accepted {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "relay accept error");
                            continue;
                        }
                    };
                    let Some(_ip_guard) = limiter.try_acquire(peer.ip()) else {
                        tracing::warn!(%peer, "per-IP connection limit hit; dropping");
                                                continue;
                    };
                    let ctx = ctx.clone();
                    let service_tx = service_tx.clone();
                    let tls = tls.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(tcp, tls, &ctx, service_tx).await {
                            tracing::debug!(error = %e, "connection ended");
                        }
                    });
                }
                _ = shutdown_rx.changed() => break,
            }
        }
        // The HTTP task's accept() parks forever once the channel closes —
        // abort it instead of relying on a graceful end.
        http.abort();
    });

    Ok(RelayServer {
        local_addr,
        shutdown: shutdown_tx,
        done,
    })
}

/// Build the rustls server config for both the service and device domains
/// (single certificate covering both, e.g. via SANs).
fn build_tls_config(
    cert: &std::path::Path,
    key: &std::path::Path,
) -> Result<Arc<rustls::ServerConfig>> {
    let certs: Vec<rustls::pki_types::CertificateDer> = {
        let file = std::fs::File::open(cert).with_context(|| "opening relay TLS cert")?;
        rustls_pemfile::certs(&mut std::io::BufReader::new(file))
            .collect::<std::io::Result<_>>()
            .context("parsing relay TLS cert PEM")?
    };
    let key = {
        let file = std::fs::File::open(key).with_context(|| "opening relay TLS key")?;
        rustls_pemfile::private_key(&mut std::io::BufReader::new(file))
            .context("parsing relay TLS key PEM")?
            .context("no private key found in relay TLS key PEM")?
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("relay TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building relay TLS config")?;
    Ok(Arc::new(server_config))
}

/// Route one accepted TCP connection.
async fn handle_conn(
    tcp: TcpStream,
    tls: Option<Arc<rustls::ServerConfig>>,
    ctx: &TunnelContext,
    service_tx: tokio::sync::mpsc::Sender<std::io::Result<ServiceIo>>,
) -> Result<()> {
    // Captured before `tcp` is consumed by the TLS acceptor; carried through
    // the device pipe so teardown lines can be attributed to a client.
    let peer = tcp.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    match tls {
        Some(server_config) => {
            // Peek the ClientHello for SNI before completing the handshake.
            let handshake =
                tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), tcp)
                    .await
                    .context("reading TLS ClientHello")?;
            let sni = handshake
                .client_hello()
                .server_name()
                .unwrap_or_default()
                .to_string();
            let tls_stream = handshake
                .into_stream(server_config)
                .await
                .context("TLS handshake")?;
            route_service_or_device(ServiceIo::new(tls_stream), sni, ctx, service_tx, &peer).await
        }
        None => {
            // Plain mode: route by the HTTP Host header via peek (bytes are
            // NOT consumed — both paths read the request from scratch).
            let host = peek_request_host(&tcp).await?;
            route_service_or_device(ServiceIo::new(tcp), host, ctx, service_tx, &peer).await
        }
    }
}

/// Peek the first request's head (without consuming) and extract the Host.
///
/// `TcpStream::peek` never consumes: every call returns the SAME leading
/// bytes. The loop therefore grows the captured window by 1 KiB per
/// attempt and keeps only the delta, so `buf` stays a faithful prefix of
/// the request head (appending the whole peek each round would duplicate
/// bytes into the buffer).
async fn peek_request_host(tcp: &TcpStream) -> Result<String> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    loop {
        let mut chunk = vec![0u8; buf.len() + 1024];
        let n = tcp.peek(&mut chunk).await.context("peeking request head")?;
        if n == 0 {
            anyhow::bail!("connection closed before a request head arrived");
        }
        if n > buf.len() {
            buf.extend_from_slice(&chunk[buf.len()..n]);
        }
        if let Some(host) = parse_host_from_head(&buf) {
            return Ok(host.to_string());
        }
        if buf.len() >= 16 * 1024 {
            anyhow::bail!("request head exceeds 16 KiB without a Host header");
        }
        // Wait briefly for more bytes; the head is usually one segment.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn parse_host_from_head(head: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(head).ok()?;
    for line in text.split("\r\n") {
        if let Some(value) = line
            .strip_prefix("Host:")
            .or_else(|| line.strip_prefix("host:"))
        {
            let host = value.trim();
            if host.is_empty() {
                return None;
            }
            return Some(host);
        }
    }
    None
}

/// Strip an optional `:port` suffix from a Host header value (SNI values
/// never carry one, so this is a no-op on the TLS path).
///
/// Real HTTP clients send `Host: relay.example.com` OR
/// `Host: relay.example.com:443` — and always a port when connecting to
/// a non-default one (loopback URLs in tests, custom ports in dev).
/// IPv6 forms keep their brackets: `[::1]:8080` → `[::1]`.
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &host[..end + 2];
        }
        return host;
    }
    if let Some(idx) = host.rfind(':') {
        let port = &host[idx + 1..];
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            return &host[..idx];
        }
    }
    host
}

/// Shared routing: service domain → control plane; device domain → byte
/// pipe into the tunnel (or 502 DEVICE_OFFLINE).
async fn route_service_or_device(
    io: ServiceIo,
    host_header_or_sni: String,
    ctx: &TunnelContext,
    service_tx: tokio::sync::mpsc::Sender<std::io::Result<ServiceIo>>,
    peer: &str,
) -> Result<()> {
    let name = host_without_port(host_header_or_sni.trim_end_matches('.'))
        .to_ascii_lowercase();
    if name == ctx.config.service_domain.to_ascii_lowercase() {
        if service_tx.send(Ok(io)).await.is_err() {
            tracing::warn!("control plane channel closed");
        }
        return Ok(());
    }

    // Device domain?
    let Some(gw_id) = ctx.config.gw_id_from_sni(&name) else {
        // Unknown SNI/Host: close.
        tracing::debug!(name = %name, "unknown relay domain; closing");
        return Ok(());
    };

    // Registered tunnel?
    let Some(handle) = ctx.registry.get(&gw_id) else {
        write_device_offline(io).await;
        return Ok(());
    };

    // Per-gateway connection cap.
    let Ok(permit) = handle.acquire_conn_permit().await else {
        tracing::warn!(gw_id = %gw_id, "per-gateway connection cap hit");
        write_device_offline(io).await;
        return Ok(());
    };

    // Open a yamux stream through the tunnel, tagged for the Gateway's
    // remote HTTP listener. The opener writes the tag byte (§5.2).
    let mut stream = match handle.open_stream(STREAM_TAG_HTTP).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(gw_id = %gw_id, %peer, error = %e, "opening tunnel stream failed");
            write_device_offline(io).await;
            return Ok(());
        }
    };
    tracing::info!(gw_id = %gw_id, %peer, "device pipe opened");

    // Splice: client ↔ yamux stream, pure bytes, until either side closes
    // or the pipe goes quiet (§5.2). The idle branch is what keeps a wedged
    // stream from sitting on a half-open device connection forever.
    let started = tokio::time::Instant::now();
    let mut client = io.compat();
    match copy_bidirectional(&mut client, &mut stream, PIPE_IDLE_TIMEOUT).await {
        Ok(stats) => tracing::info!(
            gw_id = %gw_id,
            %peer,
            dur_ms = started.elapsed().as_millis() as u64,
            in_bytes = stats.a_to_b,
            out_bytes = stats.b_to_a,
            "device pipe ended"
        ),
        Err(e) => tracing::info!(
            gw_id = %gw_id,
            %peer,
            dur_ms = started.elapsed().as_millis() as u64,
            error = %e,
            "device pipe ended"
        ),
    }
    let _ = stream.close().await;
    drop(permit);
    Ok(())
}

/// The handcrafted offline response (§5.3): no HTTP stack on the data path.
///
/// After writing the response the connection is DRAINED (bounded) before
/// closing: closing a socket with unread bytes still in the kernel receive
/// buffer makes the peer's kernel answer with RST instead of FIN, and the
/// client would surface that as a read error — never seeing the 502 we
/// just wrote. The request head was only ever `peek`ed, so at minimum the
/// whole head is still unread when we get here.
async fn write_device_offline(io: ServiceIo) {
    let response = format!(
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        DEVICE_OFFLINE_BODY.len(),
        DEVICE_OFFLINE_BODY
    );
    let mut w = io.compat();
    let _ = w.write_all(response.as_bytes()).await;

    // Bounded drain: consume what the peer already sent plus a short
    // grace window (a well-behaved client that reads the 502 closes by
    // itself, ending the loop via EOF).
    let deadline = tokio::time::Instant::now()
        + 2 * TEARDOWN_GRACE;
    let mut scratch = [0u8; 8 * 1024];
    let mut drained: usize = 0;
    while drained < 64 * 1024 {
        match tokio::time::timeout_at(deadline, w.read(&mut scratch)).await {
            Ok(Ok(0)) => break, // peer closed its side — done
            Ok(Ok(n)) => drained += n,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let _ = w.close().await;
}

// ── axum control plane ─────────────────────────────────────────────

fn build_router(ctx: TunnelContext) -> axum::Router {
    use axum::extract::{FromRequest, Path, Request};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::{delete, get};
    use axum::Json;

    #[derive(Clone)]
    struct AdminState {
        ctx: TunnelContext,
    }

    fn unauthorized() -> Response {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response()
    }

    fn admin_authorized(admin: &AdminState, req: &Request) -> bool {
        let Some(expected) = admin.ctx.config.admin_token.as_deref() else {
            return false;
        };
        if expected.is_empty() {
            return false;
        }
        req.headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
    }

    let mut router = axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/tunnel",
            get({
                let ctx = ctx.clone();
                move |ws: axum::extract::WebSocketUpgrade| {
                    let ctx = ctx.clone();
                    async move {
                        ws.on_upgrade(move |socket| async move {
                            let io = axum_ws_byte_stream(socket);
                            run_tunnel(io, ctx).await;
                        })
                    }
                }
            }),
        );

    if ctx.config.admin_token.is_some() {
        let list_devices = {
            let admin = AdminState { ctx: ctx.clone() };
            move |req: Request| async move {
                if !admin_authorized(&admin, &req) {
                    return unauthorized();
                }
                let devices: Vec<serde_json::Value> = admin
                    .ctx
                    .devices
                    .list()
                    .into_iter()
                    .map(|(gw_id, record)| {
                        serde_json::json!({
                            "gw_id": gw_id,
                            "pubkey": record.pubkey,
                            "created_at": record.created_at,
                            "last_seen_at": record.last_seen_at,
                        })
                    })
                    .collect();
                Json(serde_json::json!({ "devices": devices })).into_response()
            }
        };

        let pre_register = {
            let admin = AdminState { ctx: ctx.clone() };
            move |req: Request| async move {
                if !admin_authorized(&admin, &req) {
                    return unauthorized();
                }
                #[derive(serde::Deserialize)]
                struct Body {
                    gw_id: String,
                    pubkey: String,
                }
                let Ok(body) = axum::Json::<Body>::from_request(req, &()).await else {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": "invalid body" })),
                    )
                        .into_response();
                };
                if !acowork_core::relay::proto::is_valid_gw_id(&body.gw_id) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": "gw_id must be UUID v4" })),
                    )
                        .into_response();
                }
                match admin.ctx.devices.register(&body.gw_id, &body.pubkey) {
                    Ok(()) => (
                        StatusCode::CREATED,
                        Json(serde_json::json!({ "gw_id": body.gw_id, "registered": true })),
                    )
                        .into_response(),
                    Err(e) => (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": e.to_string() })),
                    )
                        .into_response(),
                }
            }
        };

        let revoke = {
            let admin = AdminState { ctx: ctx.clone() };
            move |Path(gw_id): Path<String>, req: Request| async move {
                if !admin_authorized(&admin, &req) {
                    return unauthorized();
                }
                // Kick the live tunnel first (GOAWAY + kill), then forget
                // the device record.
                if let Some(handle) = admin.ctx.registry.get(&gw_id) {
                    handle.goaway_and_kill("revoked by relay admin").await;
                }
                match admin.ctx.devices.revoke(&gw_id) {
                    Ok(true) => (
                        StatusCode::OK,
                        Json(serde_json::json!({ "gw_id": gw_id, "revoked": true })),
                    )
                        .into_response(),
                    Ok(false) => (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({ "error": "unknown device" })),
                    )
                        .into_response(),
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": e.to_string() })),
                    )
                        .into_response(),
                }
            }
        };

        let list_tunnels = {
            let admin = AdminState { ctx: ctx.clone() };
            move |req: Request| async move {
                if !admin_authorized(&admin, &req) {
                    return unauthorized();
                }
                let tunnels: Vec<serde_json::Value> = admin
                    .ctx
                    .registry
                    .list()
                    .into_iter()
                    .map(|(gw_id, session_id)| {
                        serde_json::json!({ "gw_id": gw_id, "session_id": session_id })
                    })
                    .collect();
                Json(serde_json::json!({ "tunnels": tunnels })).into_response()
            }
        };

        router = router
            .route(
                "/api/admin/devices",
                get(list_devices).post(pre_register),
            )
            .route("/api/admin/devices/{gw_id}", delete(revoke))
            .route("/api/admin/tunnels", get(list_tunnels));
    }
    router
}

/// Constant-time comparison so admin-token checking does not become a
/// timing oracle (mirrors the Gateway's enrollment helper).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── WS adapter (axum WebSocket → WsByteStream) ─────────────────────

use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};

struct AxumWsReader {
    stream: SplitStream<WebSocket>,
}

impl acowork_core::relay::ws_stream::WsReader for AxumWsReader {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<Option<Vec<u8>>>> {
        loop {
            match self.stream.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(None)),
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())))
                }
                Poll::Ready(Some(Ok(msg))) => match msg {
                    AxumMessage::Binary(data) => return Poll::Ready(Ok(Some(data.to_vec()))),
                    AxumMessage::Text(_) => {
                        return Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "unexpected text WS frame",
                        )))
                    }
                    AxumMessage::Ping(_) | AxumMessage::Pong(_) => continue,
                    AxumMessage::Close(_) => return Poll::Ready(Ok(None)),
                },
            }
        }
    }
}

struct AxumWsWriter {
    sink: SplitSink<WebSocket, AxumMessage>,
}

impl acowork_core::relay::ws_stream::WsWriter for AxumWsWriter {
    fn poll_send(&mut self, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<()>> {
        if let Err(e) = std::task::ready!(self.sink.poll_ready_unpin(cx)) {
            return Poll::Ready(Err(std::io::Error::other(e.to_string())));
        }
        if let Err(e) = self.sink.start_send_unpin(AxumMessage::Binary(data.to_vec().into())) {
            return Poll::Ready(Err(std::io::Error::other(e.to_string())));
        }
        match self.sink.poll_flush_unpin(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            other => other.map(|_| Ok(())),
        }
    }
}

/// Adapt an axum WebSocket (accepted `/tunnel` upgrade) into the byte
/// stream yamux requires.
pub fn axum_ws_byte_stream(ws: WebSocket) -> WsByteStream {
    let (sink, stream) = ws.split();
    WsByteStream::new(
        Box::new(AxumWsReader { stream }),
        Box::new(AxumWsWriter { sink }),
    )
}

// ── IO plumbing ────────────────────────────────────────────────────

/// Object-safe combined tokio IO for service-domain connections.
trait StreamIo: TokioAsyncRead + TokioAsyncWrite + Unpin + Send {}
impl StreamIo for TlsStream<TcpStream> {}
impl StreamIo for TcpStream {}

/// A service-domain connection as axum's `Listener::Io`.
pub struct ServiceIo(Box<dyn StreamIo>);

impl ServiceIo {
    fn new<S: StreamIo + 'static>(io: S) -> Self {
        Self(Box::new(io))
    }

    /// Convert into a futures-io compatible stream for byte piping.
    fn compat(self) -> tokio_util::compat::Compat<Box<dyn StreamIo>> {
        use tokio_util::compat::TokioAsyncReadCompatExt as _;
        self.0.compat()
    }
}

impl TokioAsyncRead for ServiceIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        TokioAsyncRead::poll_read(Pin::new(&mut *self.get_mut().0), cx, buf)
    }
}

impl TokioAsyncWrite for ServiceIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        TokioAsyncWrite::poll_write(Pin::new(&mut *self.get_mut().0), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        TokioAsyncWrite::poll_flush(Pin::new(&mut *self.get_mut().0), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        TokioAsyncWrite::poll_shutdown(Pin::new(&mut *self.get_mut().0), cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        TokioAsyncWrite::poll_write_vectored(Pin::new(&mut *self.get_mut().0), cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        TokioAsyncWrite::is_write_vectored(&*self.0)
    }
}

/// mpsc-backed "listener" feeding axum::serve with accepted control-plane
/// connections.
struct ServiceConns {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<ServiceIo>>,
}

impl axum::serve::Listener for ServiceConns {
    type Io = ServiceIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.rx.recv().await {
                Some(Ok(io)) => return (io, SocketAddr::from(([0, 0, 0, 0], 0))),
                // A failed service connection: log and keep accepting.
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "service connection failed");
                }
                // Channel closed (relay shutting down): wait forever — the
                // owner aborts this task on shutdown.
                None => std::future::pending::<()>().await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(SocketAddr::from(([0, 0, 0, 0], 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_port_strips_only_ports() {
        assert_eq!(host_without_port("relay.example.com"), "relay.example.com");
        assert_eq!(host_without_port("relay.example.com:443"), "relay.example.com");
        assert_eq!(host_without_port("127.0.0.1:19875"), "127.0.0.1");
        assert_eq!(host_without_port("[::1]:19875"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        // Non-numeric suffixes are not ports — left untouched.
        assert_eq!(host_without_port("a.b:c"), "a.b:c");
        assert_eq!(host_without_port("a:b:c"), "a:b:c");
    }
}
