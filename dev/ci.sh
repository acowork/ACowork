#!/bin/bash
# CI script for ACowork.AI
# Usage: ./dev/ci.sh [check|clippy|test|integration|smoke|all]

set -e

MODE=${1:-all}

# All cargo commands run against the core workspace; the red-line check
# below uses paths relative to the workspace root.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/../core"

echo "=== ACowork.AI CI ==="

run_check() {
    echo "Running cargo check..."
    cargo check --all
}

# ADR-055 §6.20 dependency red line: acowork-node MUST NOT depend on
# acowork-gateway (mirrors acowork-node/tests/dependency_redline.rs).
run_node_redline() {
    echo "Checking acowork-node dependency red line..."
    if grep -qE '^[[:space:]]*acowork-gateway[[:space:]]*=' acowork-node/Cargo.toml; then
        echo "ERROR: acowork-node depends on acowork-gateway (ADR-055 §6.20 red line violated)"
        exit 1
    fi
    echo "acowork-node dependency red line: OK"
}

# ADR-065 §7 #1 red line: ErrorKind::MqttState MUST NOT appear outside
# acowork-mqtt-session. Reintroducing this literal in any consumer
# crate (Node / Gateway / Desktop / Runtime) recreates the original
# wake-60s bug — the local adapter fails to unwrap the inner
# StateError::Io and re-classifies ECONNRESET as fatal E4 ConfigError.
run_mqtt_redline() {
    echo "Checking MQTT ErrorKind::MqttState red line (ADR-065 §7 #1)..."
    # Scan only Rust source. Skip the shared crate (single source of truth)
    # and the mqtt-session Cargo target directory if present.
    local offenders
    offenders=$(cd "$SCRIPT_DIR/.." && grep -rnE 'ErrorKind::MqttState' \
        --include='*.rs' \
        apps/ core/ \
        | grep -vE '^(apps|core)/acowork-mqtt-session/' \
        | grep -vE '/target/' \
        || true)
    if [ -n "$offenders" ]; then
        echo "ERROR: ErrorKind::MqttState literal found outside acowork-mqtt-session (ADR-065 §7 #1):"
        echo "$offenders"
        exit 1
    fi
    echo "MQTT ErrorKind red line: OK"
}

# ADR-009 §5.4 red line: Gateway = communication + resource management +
# reverse proxy. It MUST NOT turn an installed agent's `install_path` into a
# filesystem root. Since ADR-055 that path is a *node-local* path, so such
# access silently works on a single-machine setup and breaks (5xx) the moment
# Gateway and Node are on different machines — that is the V-A/V-B bug class:
# a second parser in the Gateway reading a directory that is not there.
#
# Ceiling, not allowlist. These are the surviving legitimate call sites
# (ADR-009 §2.2 install-time package management, the Gateway-owned avatar
# cache and its publish flow, reporting `{install_path}/workspace` back to the
# Runtime). Counts may only go DOWN: the moment one goes up, or a hit appears
# in a file that is not listed here, the boundary has been re-crossed.
# Lower a number in the same commit that removes a call site.
ADR009_FS_CEILING="
gateway/mod.rs:2
http/agents.rs:4
mqtt/dispatch.rs:3
"
run_gateway_fs_redline() {
    echo "Checking ADR-009 Gateway filesystem-isolation red line..."
    local root="$SCRIPT_DIR/../core/acowork-gateway/src"
    local hits
    hits=$(grep -rnE 'install_path' --include='*.rs' "$root" \
        | grep -E 'Path::new|PathBuf::from|\.join\(|fs::read|fs::write|canonicalize|\.exists\(' \
        | grep -vE 'install_path: ' \
        | grep -vE ':[0-9]+:[[:space:]]*//' \
        || true)

    local failed=0 rel count ceiling files f
    files=$(printf '%s\n' "$hits" | grep -oE "^$root/[^:]+" | sort -u || true)
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        rel="${f#"$root"/}"
        count=$(printf '%s\n' "$hits" | grep -cF "$f:" || true)
        ceiling=$(printf '%s\n' "$ADR009_FS_CEILING" | grep -E "^$rel:" | cut -d: -f2 || true)
        if [ -z "$ceiling" ]; then
            echo "ERROR: ${rel}: ${count} install_path filesystem access(es) — not on the ADR-009 §5.4 allowlist."
            failed=1
        elif [ "$count" -gt "$ceiling" ]; then
            echo "ERROR: ${rel}: ${count} install_path filesystem access(es), ceiling is ${ceiling}."
            failed=1
        fi
    done <<< "$files"

    if [ "$failed" -ne 0 ]; then
        echo ""
        echo "Agent-private data must be proxied through the Runtime, not read by the Gateway:"
        echo "  core/acowork-gateway/src/http/proxy.rs  ->  core/acowork-runtime/src/http/"
        exit 1
    fi
    echo "Gateway filesystem red line: OK"
}

# ADR-024 / session-listing cache: the on-disk meta layout
# (`conversations/meta/{session_id}.json`) has exactly one owner —
# core/acowork-runtime/src/conversation.rs — because session listings are
# served from an in-memory cache kept in sync by `write_session_meta` and
# `remove_session_meta`. A new place that builds or mutates a meta path can
# silently desync that cache: list a session whose file is gone, or hide one
# that exists.
#
# Ceiling, not allowlist-of-truth. These are the surviving legitimate call
# sites: test fixtures that need a meta file whose fields the real writer
# would refuse to emit. Counts may only go DOWN — lower a number in the same
# commit that removes a call site, and add nothing new.
META_LAYOUT_CEILING="
acowork-runtime/src/agent/session/session_manager.rs:1
acowork-runtime/src/http/server.rs:3
acowork-runtime/tests/conversation_session_tokens.rs:3
"
run_meta_layout_redline() {
    echo "Checking session meta layout-ownership red line (ADR-024)..."
    local root="$SCRIPT_DIR/../core"
    local hits
    hits=$(grep -rnE 'join\("meta"\)|META_DIR' --include='*.rs' "$root" \
        | grep -vE '/target/' \
        | grep -vE "^$root/acowork-runtime/src/conversation\.rs:" \
        || true)

    local failed=0 rel count ceiling files f
    files=$(printf '%s\n' "$hits" | grep -oE "^$root/[^:]+" | sort -u || true)
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        rel="${f#"$root"/}"
        count=$(printf '%s\n' "$hits" | grep -cF "$f:" || true)
        ceiling=$(printf '%s\n' "$META_LAYOUT_CEILING" | grep -E "^$rel:" | cut -d: -f2 || true)
        if [ -z "$ceiling" ]; then
            echo "ERROR: ${rel}: ${count} session-meta layout access(es) — not on the ADR-024 allowlist."
            failed=1
        elif [ "$count" -gt "$ceiling" ]; then
            echo "ERROR: ${rel}: ${count} session-meta layout access(es), ceiling is ${ceiling}."
            failed=1
        fi
    done <<< "$files"

    if [ "$failed" -ne 0 ]; then
        echo ""
        echo "Meta files must be written and removed through conversation.rs, or the"
        echo "cached session listing goes stale:"
        echo "  core/acowork-runtime/src/conversation.rs  ->  write_session_meta / remove_session_meta"
        exit 1
    fi
    echo "Session meta layout red line: OK"
}

# ADR-076 §决策 3/4: identity injection has exactly one trusted writer —
# `auth_middleware`. The reverse proxy forwards inbound headers verbatim, so
# any other `x-user-id` write is a client-asserted identity that could claim
# another user's sessions. Both the constant (`USER_SCOPE_HEADER`) and the raw
# literal (`"x-user-id"`) may only appear in their definition site and in tests
# (both live in auth_middleware.rs) — matching the literal too closes the hole
# where a new writer spells the header out as a string and bypasses the lint.
run_gateway_auth_scope_redline() {
    echo "Checking ADR-076 identity-injection red line..."
    local root="$SCRIPT_DIR/../core/acowork-gateway/src"
    local offenders
    offenders=$(grep -rnE '(USER_SCOPE_HEADER|"x-user-id")' --include='*.rs' "$root" \
        | grep -vE "^$root/http/auth_middleware\.rs:" \
        || true)
    if [ -n "$offenders" ]; then
        echo "ERROR: x-user-id scope header (constant or literal) referenced outside auth_middleware.rs (ADR-076 §决策 4):"
        echo "$offenders"
        echo "The Gateway reverse proxy must never assert a user identity itself —"
        echo "the scope is injected once, in auth_middleware, from the verified token."
        exit 1
    fi
    echo "Identity-injection red line: OK"
}

# ADR-076 §决策 12: the account API exists only under `AUTH_MODE=multi_user`.
# Its routes must stay behind an `auth_service.is_some()` branch, so `local`
# mode returns 404 rather than exposing a gated-but-registered surface.
run_gateway_auth_mode_redline() {
    echo "Checking ADR-076 auth-mode routing red line..."
    local routes="$SCRIPT_DIR/../core/acowork-gateway/src/http/routes.rs"
    # Every `auth_routes()` / `account_api` registration line must sit inside a
    # branch whose scrutinee (`state.auth_service`) is within the preceding 4
    # lines — i.e. a `match &state.auth_service { Some(_) => …, None => … }`.
    local offenders
    offenders=$(awk '
        { ctx[NR % 5] = $0 }
        /auth_api::auth_routes|account_api::/ {
            ok = 0
            for (i = 1; i <= 4; i++) {
                if (ctx[(NR - i) % 5] ~ /auth_service|AuthMode|auth_mode/) ok = 1
            }
            if (!ok) print FILENAME ":" NR ": " $0
        }
    ' "$routes" || true)
    if [ -n "$offenders" ]; then
        echo "ERROR: auth routes registered without an auth_mode branch (ADR-076 §决策 12):"
        echo "$offenders"
        echo "Register them only when the account system is active — an unregistered"
        echo "route cannot be reached by a future auth-middleware mistake."
        exit 1
    fi
    echo "Auth-mode routing red line: OK"
}

run_clippy() {
    echo "Running cargo clippy..."
    cargo clippy --all-targets -- -D warnings
    echo "Running cargo clippy for acowork-embed..."
    cargo clippy -p acowork-embed --all-targets -- -D warnings
}

run_test() {
    echo "Running cargo test..."
    # acowork-embed links to ONNX Runtime (dev/setup_ort.sh). On machines
    # without ORT installed, building its tests fails at link time — but
    # it isn't depended on by any other crate's tests, so test the rest of
    # the workspace first, then attempt the embed tests separately and
    # downgrade a link failure to a warning (matching run_smoke's policy).
    cargo test --workspace --exclude acowork-embed
    echo "Running acowork-embed tests..."
    if [ -x target/debug/acowork-embed ]; then
        echo "acowork-embed: reusing existing binary"
        cargo test -p acowork-embed --no-run 2>&1 \
            | grep -vE 'ORT_LIB_LOCATION|Downloading.*onnx' || true
    else
        cargo test -p acowork-embed --features download-ort \
            || echo "WARNING: acowork-embed tests skipped (ORT not configured)"
    fi
}

run_integration() {
    echo "=== Running node control-plane e2e tests (ADR-055 Phase 2) ==="
    cargo test -p acowork-gateway --test node_control_plane_e2e -- --test-threads=1
    echo "=== Running gateway settings API tests ==="
    cargo test -p acowork-gateway --test settings_api -- --test-threads=1
    echo "=== Running node wire-protocol golden tests ==="
    cargo test -p acowork-core --test node_proto_golden
    echo "=== Running acowork-node dependency red-line test ==="
    cargo test -p acowork-node --test dependency_redline
}

# Frontend smoke suite (dev/e2e_frontend_smoke/smoke_test.py): boots a
# real Gateway + local node agent against temp homes and exercises the
# HTTP/MQTT surface the desktop app talks to (config, sessions, workspaces,
# memory, docs, settings + Phase 5a auth). Requires debug binaries.
run_smoke() {
    echo "=== Building debug binaries for smoke tests ==="
    # acowork-embed needs ONNX Runtime (dev/setup_ort.sh) which may not
    # be installed on this machine; it is spawned as a sidecar and not
    # depended on by the other crates, so rebuild everything else and
    # reuse an existing embed binary when ORT is unavailable.
    cargo build --workspace --bins --exclude acowork-embed
    if [ -x target/debug/acowork-embed ]; then
        echo "acowork-embed: reusing existing binary (ORT not configured)"
    else
        echo "acowork-embed: building with download-ort feature..."
        cargo build -p acowork-embed --features download-ort \
            || echo "WARNING: acowork-embed build failed (embedding unavailable in smoke)"
    fi
    echo "=== Running frontend smoke tests ==="
    python3 -u "$SCRIPT_DIR/e2e_frontend_smoke/smoke_test.py"
}

case "$MODE" in
    check)
        run_gateway_fs_redline
        run_meta_layout_redline
        run_gateway_auth_scope_redline
        run_gateway_auth_mode_redline
        run_check
        ;;
    clippy)
        run_clippy
        ;;
    test)
        run_test
        ;;
    integration)
        run_integration
        ;;
    smoke)
        run_smoke
        ;;
    all)
        run_node_redline
        run_mqtt_redline
        run_gateway_fs_redline
        run_meta_layout_redline
        run_gateway_auth_scope_redline
        run_gateway_auth_mode_redline
        run_check
        run_clippy
        run_test
        run_integration
        run_smoke
        ;;
    *)
        echo "Unknown mode: $MODE"
        echo "Usage: $0 [check|clippy|test|integration|smoke|all]"
        exit 1
        ;;
esac

echo "=== CI completed successfully ==="
