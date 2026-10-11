//! Stopping an *owned* local Gateway process tree.
//!
//! The Desktop App tracks exactly one child Gateway (in
//! `AppState::gateway_process`), and only when running in **local** mode
//! (`spawn_gateway` is reachable solely from `init_local_gateway` /
//! `start_local_gateway`, both of which gate on `mode == Local`). That
//! Gateway in turn spawns the whole sidecar family: Node Agent (which hosts
//! the Runtime processes), LSP relay, embed, PM, doc, user.
//!
//! Platform behaviour of the two stop paths diverges, which is the source of
//! the "Gateway stopped but its children linger" bug on Windows:
//!
//! - **Unix** — a SIGINT to the Gateway runs *its own* signal handler, which
//!   gracefully shuts down every supervisor (Node process group, LSP relay,
//!   embed, PM, doc, user) before the Gateway exits. The tree dies with it.
//! - **Windows** — there is no equivalent signal delivery. `taskkill /T /F`
//!   force-kills the Gateway PID and walks its *current* child-process tree,
//!   but several sidecars detach from that tree: the LSP relay runs in its own
//!   process group, and the Node is spawned through a short-lived
//!   `acowork-node start` stub whose real daemon is re-parented out from under
//!   the Gateway (the Runtimes then hang off the Node, not the Gateway). A
//!   force-killed Gateway also never runs its Windows-side orphan cleanup
//!   (there is no `ps` scan), so those survivors linger — alive but useless
//!   without the Gateway.
//!
//! To make the Windows stop path honour the user's "stop the Gateway" intent,
//! after the tree kill we reap the known acowork sidecar family by image name,
//! mirroring `dev/common/build_core.ps1` and `dev/common/start_node.ps1`.
//!
//! # Scope: owned *local* Gateway only
//!
//! The full sweep ([`stop_gateway_process`]) must only run for a locally-owned
//! Gateway — never in remote/relay mode. That is guaranteed structurally: every
//! caller takes the child out of `AppState::gateway_process`, and that handle is
//! `Some` **iff** Desktop spawned the Gateway in local mode. In remote/relay mode
//! there is no child, so the tray quit dialog is skipped (Desktop just closes
//! itself) and no kill path fires.
//!
//! This matters because of the ADR-087 special case: in remote/relay mode the
//! Desktop may still spawn a *detached* `acowork-node` on this machine
//! (`create_local_node`, not tracked in `gateway_process`). A blanket by-name
//! sweep would kill that legitimate remote-connected node, so the sweep is kept
//! strictly on the owned-local shutdown paths. The lighter
//! [`stop_gateway_pid`] (tree kill, no name sweep) is used where we only need to
//! free the port without touching sidecars — e.g. switching away from local mode.

use std::process::Command;

/// Image names (without `.exe`) of the sidecar processes an owned local
/// Gateway spawns, in the order they must be reaped on Windows.
///
/// Order matters for the respawn race: kill the Node *before* the processes it
/// supervises (Runtime, LSP relay) so it cannot respawn them; the Gateway-side
/// supervisors (embed, PM, doc) are already dead with the Gateway, so they
/// never respawn. `acowork-gateway` itself is not listed — the tree kill
/// handles it, and re-running `/IM` on it could target an unrelated Gateway.
#[cfg(target_os = "windows")]
const SIDECAR_IMAGES: &[&str] = &[
    "acowork-node",
    "acowork-runtime",
    "acowork-lsp-relay",
    "acowork-embed",
    "acowork-pm",
    "acowork-doc",
    "acowork-user",
];

/// Stop an owned Gateway process tree *and* reap its detached sidecar family.
///
/// Only call this on the shutdown paths that tear down the whole owned local
/// stack (tray quit "stop", `stop_local_gateway`, app exit, `AppState::drop`).
/// Never call it in remote/relay mode — those modes have no owned child, so
/// callers naturally do not reach here; see the module docs on the ADR-087 node.
///
/// - Windows: `taskkill /T /F` the Gateway tree, then reap the detached
///   sidecar family by image name.
/// - Unix: send SIGINT so the Gateway's own signal handler cascades the
///   shutdown to every supervisor.
///
/// Callers are responsible for taking the `Child` handle out of the mutex and
/// reaping it (`Child::wait`) afterwards to avoid zombies.
pub(crate) fn stop_gateway_process(pid: u32) {
    stop_gateway_pid(pid);
    #[cfg(target_os = "windows")]
    reap_detached_sidecars();
}

/// Stop an owned Gateway process tree WITHOUT the sidecar name sweep.
///
/// Use where only the Gateway/port must go down and lingering sidecars are
/// acceptable or undesirable to touch (e.g. switching from local to remote mode,
/// where a Desktop-created ADR-087 node may be repointing at the remote Gateway).
///
/// - Windows: `taskkill /T /F` the Gateway tree.
/// - Unix: send SIGINT so the Gateway's own signal handler cleans up.
pub(crate) fn stop_gateway_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Send SIGINT so the Gateway's signal handler cleans up its children.
        let _ = Command::new("kill").args(["-INT", &pid.to_string()]).output();
    }
}

/// Best-effort `taskkill /IM` sweep of the sidecar family that the Windows tree
/// kill cannot reach (detached process groups / re-parented daemons).
#[cfg(target_os = "windows")]
fn reap_detached_sidecars() {
    for image in SIDECAR_IMAGES {
        let target = format!("{image}.exe");
        let output = Command::new("taskkill")
            .args(["/IM", &target, "/T", "/F"])
            .output();
        match output {
            // `/IM` exits non-zero when no such process exists — that is the
            // common, benign case (the sidecar was in the tree or never
            // spawned), so only log the rare successful reaps.
            Ok(o) if o.status.success() => {
                tracing::info!(image, "Reaped detached sidecar process left by the Gateway");
            }
            _ => {}
        }
    }
}
