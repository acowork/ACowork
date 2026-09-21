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
//! mode is not a wrong response, it is a Gateway that *starts* in the mode
//! nobody asked for. A unit test cannot observe "refused to start".
//!
//! Assertions are deliberately limited to what a test can see without an HTTP
//! client: exit status, stderr, a listening socket, and the `--home` tree.
//! Adding a client dependency to re-prove the router's own tests would not
//! make the process-level story any stronger.
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

/// Fail-fast and clean-shutdown paths are quick; this bounds the wait.
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

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

    /// Wait for the process to exit on its own. `None` on timeout.
    fn wait_for_exit(&mut self) -> Option<Exit> {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return Some(Exit {
                    code: status.code(),
                    stderr: self.stderr(),
                });
            }
            sleep(Duration::from_millis(100));
        }
        None
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        // Only ever our own child — never a pattern match on process names.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Exit {
    code: Option<i32>,
    stderr: String,
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

// ── Refusing to start ──────────────────────────────────────────────────

/// ADR-076 §决策 12 / §7.5: `multi_user` with an empty account store and no
/// bootstrap administrator has no way to log in — the binary must fail fast
/// rather than come up as a Gateway nobody can enter.
#[test]
fn multi_user_without_bootstrap_admin_refuses_to_start() {
    let home = temp_home("nobootstrap");
    let (http, mqtt) = (free_port(), free_port());
    let mut gw = spawn(&home, http, mqtt, &["--auth-mode", "multi_user"]);

    let exit = gw.wait_for_exit().expect(
        "gateway must exit on its own — a running process here means the \
         bootstrap_admin check was skipped",
    );
    assert_ne!(exit.code, Some(0), "expected a non-zero exit status");
    assert!(
        exit.stderr.contains("bootstrap_admin"),
        "stderr must name the missing setting, got:\n{}",
        exit.stderr
    );
    // It must say *why* it gave up, not merely that it did.
    assert!(
        exit.stderr.contains("multi_user"),
        "stderr must name the mode, got:\n{}",
        exit.stderr
    );
    assert!(
        !gw.data_file("accounts.json").exists(),
        "a failed boot must not leave an account store behind"
    );
}

// ── Booting in multi_user ──────────────────────────────────────────────

/// The TOML path, not the CLI flag: `auth_mode` plus `bootstrap_admin` from
/// `gateway.toml` must produce a serving Gateway with a usable first admin.
#[test]
fn multi_user_from_config_bootstraps_an_admin_and_serves() {
    let home = temp_home("bootstrap");
    let (http, mqtt) = (free_port(), free_port());
    let config = write_config(
        &home,
        &format!(
            r#"
vault_dir = "{home}/data/vault"
packages_dir = "{home}/config/packages"
data_dir = "{home}/data"

auth_mode = "multi_user"

[multi_user.bootstrap_admin]
username = "root"
password = "s3cret123"
display_name = "Root"
"#,
            home = home.display()
        ),
    );

    let mut gw = spawn(
        &home,
        http,
        mqtt,
        &["--config-path", config.to_str().unwrap()],
    );
    gw.wait_until_serving();

    let accounts = gw.data_file("accounts.json");
    assert!(
        accounts.exists(),
        "a booted multi_user Gateway must persist its account store at {}",
        accounts.display()
    );
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
        !gw.data_file("accounts.json").exists(),
        "local mode must not create an account store"
    );
    assert!(
        !gw.data_file("auth").exists(),
        "local mode must not create the auth secret directory"
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
