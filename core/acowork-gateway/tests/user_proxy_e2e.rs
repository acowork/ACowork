//! ADR-084 §7.2 — `user_proxy` end-to-end at the **process** level.
//!
//! The in-process router tests (`http::user_proxy::tests`) and the
//! `test_support::gateway_identity_shim` cover the surface in isolation, but
//! none of them cross a real `acowork-gateway` ↔ `acowork-user` boundary.
//! That is the gap this file fills: it spawns the actual Gateway binary (which
//! in turn supervises the actual `acowork-user` binary) and walks the public
//! paths the Desktop walks, so the chain
//!
//! ```text
//! HTTP → auth_middleware (Ed25519) → user_proxy → user service → 200
//! ```
//!
//! is exercised as a single black box.
//!
//! Mirror of `auth_mode_e2e.rs`'s shape: dependency-free HTTP, ephemeral ports,
//! per-test temp homes, child killed on drop.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

const BOOT_TIMEOUT: Duration = Duration::from_secs(30);

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().unwrap().port()
}

/// Locate the `acowork-gateway` binary next to this test executable.
fn gateway_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target/debug dir");
    let name = if cfg!(windows) {
        "acowork-gateway.exe"
    } else {
        "acowork-gateway"
    };
    let candidate = dir.join(name);
    if candidate.exists() {
        candidate
    } else {
        dir.parent().expect("target root").join(name)
    }
}

#[allow(dead_code)] // kept for future per-test diagnostics
struct Gateway {
    child: Child,
    home: PathBuf,
    http_port: u16,
    stderr: Arc<Mutex<String>>,
}

impl Gateway {
    fn stderr(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn wait_until_serving(&mut self) {
        let deadline = Instant::now() + BOOT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                panic!(
                    "gateway exited before serving ({status}); stderr:\n{}",
                    self.stderr()
                );
            }
            if std::net::TcpStream::connect(("127.0.0.1", self.http_port)).is_ok() {
                if let Some(status) = self.child.try_wait().expect("try_wait") {
                    panic!("gateway exited right after binding ({status})");
                }
                return;
            }
            sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        panic!(
            "gateway did not serve on port {} within {BOOT_TIMEOUT:?}; stderr:\n{}",
            self.http_port,
            self.stderr()
        );
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        kill_tree(&mut self.child);
    }
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .output();
    let _ = child.kill();
    let _ = child.wait();
}

fn temp_home(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "acowork-test-user-proxy-{tag}-{}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("config")).expect("create home");
    home
}

fn spawn(home: &Path, http_port: u16, mqtt_port: u16, extra: &[&str]) -> Gateway {
    let mut cmd = Command::new(gateway_binary());
    cmd.arg("--daemon")
        .arg("--home")
        .arg(home)
        .arg("--addr")
        .arg(format!("127.0.0.1:{http_port}"))
        .arg("--mqtt-addr")
        .arg(format!("127.0.0.1:{mqtt_port}"))
        .arg("--no-spawn-local-node")
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn acowork-gateway");
    let stderr = Arc::new(Mutex::new(String::new()));
    if let Some(pipe) = child.stderr.take() {
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                if let Ok(line) = line
                    && let Ok(mut buf) = sink.lock()
                {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        });
    }
    Gateway {
        child,
        home: home.to_path_buf(),
        http_port,
        stderr,
    }
}

fn write_config(home: &Path, body: &str) -> PathBuf {
    let path = home.join("config").join("gateway.toml");
    fs::write(&path, body).expect("write gateway.toml");
    path
}

/// Minimal raw-socket HTTP/1.1 GET → `(status, body, raw_headers_block)`.
///
/// Returned headers are scanned by `header_pred` so callers can pull out
/// `Retry-After` and similar without an HTTP client dependency.
fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    extra_headers: &[(&str, &str)],
) -> (u16, String, String) {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("set read timeout");
    let len = body.map(|b| b.len()).unwrap_or(0);
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n"
    );
    if body.is_some() {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {len}\r\n"));
    }
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).expect("write request head");
    if let Some(b) = body {
        stream.write_all(b).expect("write body");
    }
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_else(|| (raw.clone(), String::new()));
    (status, body, head)
}

fn http_get(port: u16, path: &str) -> (u16, String, String) {
    http_request(port, "GET", path, None, &[])
}

fn http_post_json(port: u16, path: &str, body: &[u8]) -> (u16, String, String) {
    http_request(port, "POST", path, Some(body), &[])
}

fn header_value<'a>(raw_headers: &'a str, name: &str) -> Option<&'a str> {
    // Case-insensitive header lookup over the raw response head.
    for line in raw_headers.split("\r\n").skip(1) {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim());
        }
    }
    None
}

/// Pull a string field out of a JSON body without pulling a real JSON crate
/// into test deps. Good enough for the small flat shapes the user service
/// returns here.
fn json_string_field<'a>(body: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!("\"{field}\":");
    let start = body.find(&needle)? + needle.len();
    let rest = body[start..].trim_start();
    if let Some(s) = rest.strip_prefix('"') {
        let end = s.find('"')?;
        return Some(&s[..end]);
    }
    None
}

/// Wait until the user service is actually ready to **verify tokens** —
/// i.e. the supervisor has finished its post-spawn setup, including loading
/// the Ed25519 public key from `{user_data_dir}/auth/ed25519.pub`.
///
/// `/api/status` is *not* a reliable signal here: `requires_setup` is
/// serialised via `gw.user_snapshot.as_ref().is_some_and(|s| s.requires_setup)`,
/// which is `false` while the snapshot is `None` (the supervisor's pre-setup
/// state). The same trap bit a phase of M3 dev (see dev-plan §3-M3 "未覆盖"
/// note). The only externally observable "verifier is loaded" signal is
/// `auth_middleware` switching from a 503 ("authentication is unavailable")
/// to a 401 (token rejected) on a request carrying any bearer header — so
/// this helper just polls that transition.
fn wait_for_user_service_ready(port: u16) {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    let mut last = (0u16, String::new());
    while Instant::now() < deadline {
        let (code, body, _) = http_request(
            port,
            "GET",
            "/api/users",
            None,
            &[("Authorization", "Bearer probe-token")],
        );
        // Anything other than 503 means the gate has a verifier (and is
        // parsing the token). 401 is the expected outcome here (invalid
        // token); 200 means the user service decided to answer — fine too.
        if code != 503 {
            return;
        }
        last = (code, body);
        sleep(Duration::from_millis(100));
    }
    panic!(
        "user service did not become ready (verifier still None) within {BOOT_TIMEOUT:?}; \
         last probe: {} body={}",
        last.0, last.1
    );
}

/// Wait until `user_proxy` is serving `path` for an **unauthenticated**
/// request — used in `local` mode (where `auth_middleware` is a no-op).
/// The proxy's not-ready branch is the only 503 on this path, so a non-503
/// is enough to claim readiness.
fn wait_until_user_proxy_serves(port: u16, path: &str) {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    let mut last = (0u16, String::new());
    while Instant::now() < deadline {
        let (code, body, _) = http_get(port, path);
        if code != 503 {
            return;
        }
        last = (code, body);
        sleep(Duration::from_millis(100));
    }
    panic!(
        "user proxy did not start serving within {timeout:?}; last probe {}: {} body={}",
        last.0, path, last.1,
        timeout = BOOT_TIMEOUT
    );
}

/// The default `acowork-user` port (ADR-084 §决策 5). The supervisor
/// auto-increments on collision but defaults to this; tests that need to
/// debug the user service directly (rather than through the Gateway) use it.
fn default_user_port() -> u16 {
    18083
}

/// Probe the user service's `/health` directly. Returns `(actual_port, body)`
/// where `body` is `Some(json)` if `/health` returned 200, else `None`. The
/// scan starts at `start_port` and walks forward until either a service
/// answers or we run out of patience — this matches the supervisor's own
/// auto-increment window without depending on it.
fn probe_user_service(start_port: u16) -> (u16, Option<String>) {
    for offset in 0..20u16 {
        let port = start_port + offset;
        let (code, body, _) = http_get(port, "/health");
        if code == 200 {
            return (port, Some(body));
        }
    }
    (start_port, None)
}

// ── multi_user full chain ──────────────────────────────────────────────

/// ADR-084 §7.2 / dev-plan §7: under `multi_user` a real login against
/// `POST /api/auth/login` must yield an access token, and that token must
/// then carry the caller past the Gateway into the user service for a
/// `GET /api/users` that returns 200. The whole point of the test is the
/// chain — the in-process `gateway_identity_shim` cannot prove it.
#[test]
fn multi_user_login_then_users_via_proxy() {
    let home = temp_home("login-chain");
    let (http, mqtt) = (free_port(), free_port());
    // Forward slashes for the same TOML escape reason `auth_mode_e2e` cites.
    let home_s = home.display().to_string().replace('\\', "/");
    let config = write_config(
        &home,
        &format!(
            r#"
vault_dir = "{home}/data/vault"
packages_dir = "{home}/config/packages"
data_dir = "{home}/data"

auth_mode = "multi_user"

[user]
config = "{home}/acowork-user.toml"
"#,
            home = home_s
        ),
    );
    fs::write(
        home.join("acowork-user.toml"),
        r#"
[bootstrap_admin]
username = "root"
password = "s3cret123"
display_name = "Root"
"#,
    )
    .expect("write acowork-user.toml");

    let mut gw = spawn(
        &home,
        http,
        mqtt,
        &["--config-path", config.to_str().unwrap()],
    );
    gw.wait_until_serving();

    // Probe the user service's /health directly. The supervisor reads the
    // verifier from `details.data_dir`; if those two diverge (e.g. under
    // `--home`, where the parent env is set via `set_var`), the verifier
    // load fails and every authenticated request 503s while /api/status
    // *still* sees `requires_setup:false` (the snapshot is set, the
    // verifier is not).
    let (user_port, user_health) = probe_user_service(default_user_port());
    let data_dir = user_health
        .as_deref()
        .and_then(|b| json_string_field(b, "data_dir"))
        .unwrap_or("<not reachable>");
    eprintln!(
        "[multi_user_login_then_users_via_proxy] user service probe: port={user_port} \
         data_dir={data_dir}\n[stderr]\n{}",
        gw.stderr()
    );

    // Wait for the user service to actually verify tokens. /api/status's
    // `requires_setup` is *false by default* (`gw.user_snapshot` is None
    // until the supervisor finishes setup), so a check on that flag alone
    // returns immediately — long before the Ed25519 verifier is loaded and
    // `auth_middleware` will accept any request. The only observable signal
    // for the latter is a 401 on a request with a syntactically valid but
    // content-wrong bearer: with the verifier in, the gate runs and
    // rejects; without it, the gate 503s before parsing.
    wait_for_user_service_ready(http);

    // Login → access token.
    let body = serde_json::to_vec(&serde_json::json!({
        "username": "root",
        "password": "s3cret123",
    }))
    .expect("serialize login");
    let (code, body, _) = http_post_json(http, "/api/auth/login", &body);
    assert_eq!(code, 200, "bootstrap admin login must succeed; body:\n{body}");
    let access = json_string_field(&body, "access_token")
        .expect("login response must carry access_token; body:\n{body}")
        .to_string();

    // Token-bearing GET /api/users must proxy through to the user service
    // and answer 200 (not 401, not 503). This is the gate-and-proxy chain
    // the in-process tests cannot reach.
    let (code, body, _) = http_request(
        http,
        "GET",
        "/api/users",
        None,
        &[("Authorization", &format!("Bearer {access}"))],
    );
    assert_eq!(
        code, 200,
        "GET /api/users with a fresh login token must return 200; body:\n{body}"
    );
    assert!(
        body.contains("root"),
        "the admin's own profile must be in the list (the proxy injected the right identity); body:\n{body}"
    );
}

// ── local mode ────────────────────────────────────────────────────────

/// ADR-084 §1.4 / §决策 6: under `local` the user service runs for
/// profiles/avatars only — `/api/users` must answer, but `/api/auth/*`
/// must NOT (the auth routes are simply not registered; the public path
/// distinction is real, not a 200/401 split).
#[test]
fn local_mode_profile_via_proxy_works_but_login_returns_404() {
    let home = temp_home("local-routes");
    let (http, mqtt) = (free_port(), free_port());
    let mut gw = spawn(&home, http, mqtt, &[]);
    gw.wait_until_serving();

    // Local mode: auth_middleware is a no-op, so a plain unauthenticated
    // `GET /api/users` reaches the proxy. The 503 here is the proxy's
    // "user_process = None" branch — a probe on /api/users therefore
    // doubles as "user service spawned" while skipping the verifier-load
    // window that complicates the multi_user wait above.
    wait_until_user_proxy_serves(http, "/api/users");

    let (code, body, _) = http_get(http, "/api/users");
    assert_eq!(
        code, 200,
        "GET /api/users under local must proxy to the user service (200); body:\n{body}"
    );

    // Auth routes are absent in local — Gateway returns 404 (route not
    // matched) rather than 401 (the middleware being strict). The public
    // path of `auth_mode_e2e::multi_user_without_bootstrap_admin_serves_restricted_mode`
    // already pins the 403 setup_required surface; this one pins the
    // route-does-not-exist surface under local.
    let login_body = serde_json::to_vec(&serde_json::json!({
        "username": "root",
        "password": "irrelevant",
    }))
    .expect("serialize login body");
    let (code, _body, _) = http_post_json(http, "/api/auth/login", &login_body);
    assert_eq!(
        code, 404,
        "POST /api/auth/login must NOT be registered under local (404)"
    );
}

// ── not-ready semantics ───────────────────────────────────────────────

/// ADR-084 §决策 5 / user_proxy §not-ready: when `[user].enabled=false`
/// the supervisor never spawns and `user_process` stays `None`. Every
/// `/api/auth/*` and `/api/users/*` request must then answer **503** with
/// `Retry-After: 2` — the Desktop `with503Retry` keys off that header.
#[test]
fn user_service_disabled_returns_503_with_retry_after() {
    let home = temp_home("disabled");
    let (http, mqtt) = (free_port(), free_port());
    let home_s = home.display().to_string().replace('\\', "/");
    let config = write_config(
        &home,
        &format!(
            r#"
vault_dir = "{home}/data/vault"
packages_dir = "{home}/config/packages"
data_dir = "{home}/data"

auth_mode = "multi_user"

[user]
enabled = false
config = "{home}/acowork-user.toml"
"#,
            home = home_s
        ),
    );
    fs::write(
        home.join("acowork-user.toml"),
        r#"
[bootstrap_admin]
username = "root"
password = "s3cret123"
display_name = "Root"
"#,
    )
    .expect("write acowork-user.toml");

    let mut gw = spawn(
        &home,
        http,
        mqtt,
        &["--config-path", config.to_str().unwrap()],
    );
    gw.wait_until_serving();

    let (code, body, headers) = http_get(http, "/api/users");
    assert_eq!(
        code, 503,
        "GET /api/users with user supervisor disabled must return 503; body:\n{body}"
    );
    assert_eq!(
        header_value(&headers, "Retry-After"),
        Some("2"),
        "503 must carry Retry-After: 2 for with503Retry"
    );

    let (code, _body, headers) = http_get(http, "/api/auth/me");
    assert_eq!(
        code, 503,
        "GET /api/auth/me with user supervisor disabled must also return 503"
    );
    assert_eq!(
        header_value(&headers, "Retry-After"),
        Some("2"),
        "503 on /api/auth/me must also carry Retry-After: 2"
    );
}