//! ADR-076 §7.5 — deployment-mode acceptance, at the **process** level.
//!
//! The route surface is already pinned by in-process router tests
//! (`http::auth_api::tests`, `http::account_api::tests`, `http::chat_api::tests`),
//! and the mode-resolution truth table by `auth::mode` unit tests. What none
//! of those can reach is the wiring in between: CLI flag / TOML → config →
//! `Gateway::new` → exit code, stderr, and what lands on disk before the
//! server ever answers.
//!
//! That gap is the whole point of `AUTH_MODE` (ADR-076 §决策 12): the failure
//! mode is not a wrong response, it is a Gateway that *boots into the mode
//! nobody asked for* — or (v2/v3) boots into **restricted mode** because
//! nobody has set the first password yet. Both are process-level states.
//!
//! Assertions stay dependency-free: exit status, stderr, a listening socket,
//! the `--home` tree, and one hand-rolled raw-socket GET. The v2/v3 contract
//! ("403 `setup_required`, not 401") is a *response*, so an exit code alone
//! can no longer state it — but pulling in an HTTP client to re-prove the
//! router's own tests would not make the process-level story any stronger.
//!
//! Each test gets its own temp home and its own ephemeral ports; the child is
//! killed by the [`Gateway`] guard even when an assertion panics, so a failing
//! run cannot leave a stray daemon behind.

use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long a *successful* boot may take before we call it a hang. Generous:
/// the point is to catch "never binds", not to benchmark startup.
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Pick a free TCP port. The listener is dropped immediately, so the port is
/// only *probably* free — hence a fresh pair per test and a retry-free design
/// (a collision surfaces as a boot failure with stderr attached).
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

/// A running (or finished) Gateway plus its temp home. Dropping the guard
/// kills the child, which is what keeps a panic from leaking a daemon.
struct Gateway {
    child: Child,
    home: PathBuf,
    http_port: u16,
    /// Drained by a reader thread, so the child can never block on a full
    /// pipe and a panic can still show what it said before dying.
    stderr: Arc<Mutex<String>>,
}

impl Gateway {
    /// `data/` (or any other) path inside this Gateway's private home.
    fn data_file(&self, relative: &str) -> PathBuf {
        self.home.join("data").join(relative)
    }

    fn stderr(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Wait for the HTTP port to accept a connection — proof the binary
    /// booted far enough to serve.
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
                // One more poll: a start refusal can race the bind.
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

/// Kill the child **and every process it spawned**.
///
/// `Child::kill()` is TerminateProcess on Windows: it reaps only the direct
/// child, so the services that child hosted (embed / node / doc / pm / user)
/// survive until their ADR-018 watchdog notices the Gateway is gone (5 min),
/// holding their ports and — on Windows — a lock on `acowork-*.exe` that
/// fails the next `cargo build`. `taskkill /T` reaps the tree with it.
///
/// Only ever our own child — never a pattern match on process names. Unix
/// needs no equivalent here: the watchdog is the designed backstop and a
/// running binary does not lock its own path there.
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
        "acowork-test-authmode-{tag}-{}-{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("config")).expect("create home");
    home
}

/// Spawn the binary with a private home and private ports.
///
/// `--no-spawn-local-node` keeps the test from depending on an
/// `acowork-node` binary being present; it is orthogonal to the auth-mode
/// behaviour under test.
fn spawn(home: &Path, http_port: u16, mqtt_port: u16, extra: &[&str]) -> Gateway {
    spawn_on(home, &format!("127.0.0.1:{http_port}"), http_port, mqtt_port, extra)
}

/// `bind` is passed separately because `--addr` takes a single value: a
/// second occurrence is a clap error, so the public-bind case cannot simply
/// append another one.
fn spawn_on(
    home: &Path,
    bind: &str,
    http_port: u16,
    mqtt_port: u16,
    extra: &[&str],
) -> Gateway {
    let mut cmd = Command::new(gateway_binary());
    cmd.arg("--daemon")
        .arg("--home")
        .arg(home)
        .arg("--addr")
        .arg(bind)
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
                match line {
                    Ok(line) => {
                        if let Ok(mut buf) = sink.lock() {
                            buf.push_str(&line);
                            buf.push('\n');
                        }
                    }
                    Err(_) => break,
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

/// Write `gateway.toml` and return the path to pass as `--config-path`.
fn write_config(home: &Path, body: &str) -> PathBuf {
    let path = home.join("config").join("gateway.toml");
    fs::write(&path, body).expect("write gateway.toml");
    path
}

// ── First-boot restricted mode (ADR-076 §决策 12 v2 / v3) ───────────────

/// Minimal raw-socket HTTP/1.1 GET → `(status, body)`. Dependency-free on
/// purpose (see the module doc).
fn http_get(port: u16, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .expect("send request");
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw); // the peer closes; the timeout is a backstop
    let code = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (code, body)
}

/// Poll `/api/status` until `pred` accepts the body, or panic.
///
/// Since ADR-084 the state this endpoint carries can *lag* the listen socket:
/// `requires_setup` / `registration_open` come from the snapshot the
/// supervisor reads off the supervised user service's `/health`, and that
/// service is spawned asynchronously after the Gateway binds. Waiting for the
/// socket is therefore no longer enough to observe a settled state.
fn wait_for_status(port: u16, pred: impl Fn(&str) -> bool, what: &str) -> String {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (code, body) = http_get(port, "/api/status");
        if code == 200 && pred(&body) {
            return body;
        }
        last = body;
        sleep(Duration::from_millis(100));
    }
    panic!("{what} was not observed within {BOOT_TIMEOUT:?}; last /api/status body:\n{last}");
}

/// Wait for a file to appear (the user service writes its store at its own
/// boot, which the Gateway does not serialise against).
fn wait_for_file(path: &Path, what: &str) {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    while !path.exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(100));
    }
    assert!(path.exists(), "{what} did not appear at {}", path.display());
}

/// ADR-076 §决策 12 v2/v3: `multi_user` with an empty account store and no
/// bootstrap administrator must **not** refuse to start any more (v1 did, and
/// the failure was invisible behind `build_macos.sh`'s `> /dev/null`). It
/// seeds a passwordless `admin`, boots, and serves **restricted mode**:
/// `/health` and `/api/status` answer, everything else is 403
/// `setup_required` — *including* a request with no token at all, which is
/// exactly why the gate has to sit outside `auth_middleware` (a 401 here
/// would be indistinguishable from a bad token).
#[test]
fn multi_user_without_bootstrap_admin_serves_restricted_mode() {
    let home = temp_home("nobootstrap");
    let (http, mqtt) = (free_port(), free_port());
    let mut gw = spawn(&home, http, mqtt, &["--auth-mode", "multi_user"]);
    gw.wait_until_serving();

    let (code, _) = http_get(http, "/api/status");
    assert_eq!(code, 200, "restricted mode must answer /api/status");
    wait_for_status(
        http,
        |body| body.contains("\"requires_setup\":true"),
        "the restricted-mode flag on /api/status",
    );

    let (code, body) = http_get(http, "/api/users");
    assert_eq!(
        code, 403,
        "a token-less /api/users must be 403 setup_required, never 401; body:\n{body}"
    );
    assert!(
        body.contains("setup_required"),
        "the 403 must name the reason; body:\n{body}"
    );

    // The seed *is* written this time — that is the whole point of v2/v3.
    //
    // ADR-084: no longer inside `{gateway.data_dir}` — the account store
    // belongs to the user service, whose data dir `--home` moves along with
    // everything else.
    let accounts = gw.home.join("acowork-user").join("accounts.json");
    wait_for_file(&accounts, "the seeded account store");
    let raw = fs::read_to_string(&accounts).expect("read accounts.json");
    assert!(
        raw.contains("\"admin\""),
        "the seed must be the built-in admin; got:\n{raw}"
    );

    // The operator is pointed at the setup paths on stderr, and the same text
    // goes to the log file (the motivating path swallows stderr).
    assert!(
        gw.stderr().contains("admin-setup"),
        "stderr must point at the setup paths; got:\n{}",
        gw.stderr()
    );
}

// ── Booting in multi_user ──────────────────────────────────────────────

/// The TOML path, not the CLI flag: `auth_mode` plus `bootstrap_admin` from
/// `gateway.toml` must produce a serving Gateway with a usable first admin.
#[test]
fn multi_user_from_config_bootstraps_an_admin_and_serves() {
    let home = temp_home("bootstrap");
    let (http, mqtt) = (free_port(), free_port());
    // Forward slashes: `Path::display()` yields backslashes on Windows, and a
    // lone backslash in a TOML basic string is an escape introducer, not a
    // path separator.
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
    // ADR-084: `[multi_user]` describes the account system, and that system
    // now lives in the user service — so the bootstrap admin is configured in
    // *its* file, which the supervisor forwards as `--config`.
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

    let accounts = gw.home.join("acowork-user").join("accounts.json");
    wait_for_file(&accounts, "the bootstrapped account store");
    let raw = fs::read_to_string(&accounts).expect("read accounts.json");
    assert!(
        raw.contains("root"),
        "the bootstrap admin must have been written, got:\n{raw}"
    );
    // The bootstrap password must never be stored in the clear.
    assert!(
        !raw.contains("s3cret123"),
        "accounts.json must hold a password hash, not the password"
    );
}

// ── Booting in local ───────────────────────────────────────────────────

/// ADR-076 §7.5: `local` must be *inert* — no `accounts.json`, no
/// `data/auth/`, and (per §决策 12) the account routes are never registered.
/// Loopback bind with no explicit flag is the inferred-local case.
#[test]
fn local_mode_leaves_no_account_state_behind() {
    let home = temp_home("local");
    let (http, mqtt) = (free_port(), free_port());
    let mut gw = spawn(&home, http, mqtt, &[]);
    gw.wait_until_serving();

    assert!(
        !gw.home.join("acowork-user").join("accounts.json").exists(),
        "local mode must not create an account store (ADR-084 §1.4)"
    );
    assert!(
        !gw.home.join("acowork-user").join("auth").exists(),
        "local mode must not create the signing-key directory (ADR-084 §1.4)"
    );
}

/// The dangerous direction: a *public* bind with the mode explicitly pinned
/// to `local`. Getting this backwards exposes an unauthenticated Gateway on
/// a reachable interface, and it is the one case where the flag has to win
/// over the inference.
#[test]
fn an_explicit_local_mode_overrides_a_public_bind() {
    let home = temp_home("explicitlocal");
    let (http, mqtt) = (free_port(), free_port());
    let mut gw = spawn_on(
        &home,
        &format!("0.0.0.0:{http}"),
        http,
        mqtt,
        &["--auth-mode", "local"],
    );
    // `127.0.0.1:<port>` still connects when the listener is on `0.0.0.0`.
    gw.wait_until_serving();

    assert!(
        !gw.data_file("accounts.json").exists(),
        "an explicit --auth-mode local must stay local, whatever the bind"
    );
}
