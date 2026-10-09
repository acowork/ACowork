#!/usr/bin/env bash
# ACowork LSP install script: marksman (Markdown)
# Phases: Install → Verify → Health Check
# marksman is a standalone binary (not npm). Defaults to stdio mode.

set -euo pipefail

# Cross-platform timeout helper (macOS doesn't ship `timeout`).
_timeout() {
    local seconds="$1"; shift
    if command -v gtimeout &>/dev/null; then
        gtimeout "$seconds" "$@"
    elif command -v timeout &>/dev/null; then
        timeout "$seconds" "$@"
    else
        perl -e 'alarm shift; exec @ARGV' -- "$seconds" "$@"
    fi
}

BINARY="marksman"

# Move a downloaded binary into place. `elevate` is either "" or "sudo".
# sudo runs with -n (non-interactive): the install script has no TTY, so a
# password prompt would hang until the harness idle timeout kills it.
_place_binary() {
    local src="$1" dest="$2" elevate="${3:-}"
    if [[ -n "$elevate" ]]; then
        sudo -n true &>/dev/null || return 1
        sudo -n cp "$src" "$dest" && sudo -n chmod 755 "$dest" || return 1
    else
        cp "$src" "$dest" && chmod 755 "$dest" || return 1
    fi
    return 0
}

# ── Phase 1: Install ──────────────────────────────────────────────────
install() {
    echo "[1/3] Installing marksman..."

    # Already on PATH?
    if command -v "$BINARY" &>/dev/null; then
        echo "marksman already on PATH at $(command -v "$BINARY")"
        return 0
    fi

    # Not on PATH — search common locations
    for d in "$HOME/.local/bin" "/usr/local/bin" "/usr/bin"; do
        local candidate="$d/$BINARY"
        if [[ -x "$candidate" ]]; then
            echo "Found marksman at $candidate — adding $d to PATH..."
            case ":$PATH:" in
                *:"$d":*) ;;
                *) export PATH="$PATH:$d" ;;
            esac
            return 0
        fi
    done

    # Not installed — try brew or download
    if [[ "$(uname -s)" == "Darwin" ]] && command -v brew &>/dev/null; then
        brew install marksman
    elif command -v curl &>/dev/null; then
        echo "Downloading marksman from GitHub releases..."
        local os arch path
        case "$(uname -s)" in
            Linux)  os="linux" ;;
            Darwin) os="macos" ;;
            *)      echo "ERROR: Unsupported OS" && exit 1 ;;
        esac
        case "$(uname -m)" in
            x86_64)  arch="x64" ;;
            aarch64) arch="arm64" ;;
            arm64)   arch="arm64" ;;
            *)       echo "ERROR: Unsupported architecture" && exit 1 ;;
        esac
        path="artempyanykh/marksman/releases/latest/download/marksman-${os}-${arch}"
        # The release asset itself is served from release-assets.githubusercontent.com,
        # which is routinely slow or stalls from mainland China (github.com itself
        # stays reachable). Try a GitHub proxy first, then GitHub direct.
        local tmp url downloaded=0
        tmp="$(mktemp)"
        for url in \
            "https://ghfast.top/https://github.com/${path}" \
            "https://github.com/${path}"; do
            echo "Downloading $url"
            if curl -fL --connect-timeout 10 --max-time 300 -o "$tmp" "$url"; then
                downloaded=1
                break
            fi
            echo "WARN: download failed, trying next source..."
        done
        if [[ "$downloaded" -ne 1 ]]; then
            rm -f "$tmp"
            echo "ERROR: Download failed from all sources."
            echo "Install manually: https://github.com/artempyanykh/marksman/releases"
            exit 1
        fi

        # Try a system-wide install first (requires sudo on Linux).
        # Fall back to user-local install (~/.local/bin) which is commonly on PATH.
        local dest="" elevate="" installed=0
        if [[ -w "/usr/local/bin" ]]; then
            dest="/usr/local/bin/marksman"
        elif [[ -d /usr/local/bin ]] && command -v sudo &>/dev/null; then
            dest="/usr/local/bin/marksman"
            elevate="sudo"
        fi
        if [[ -n "$dest" ]] && _place_binary "$tmp" "$dest" "$elevate"; then
            installed=1
        fi
        if [[ "$installed" -ne 1 ]]; then
            dest="$HOME/.local/bin/marksman"
            mkdir -p "$HOME/.local/bin"
            if _place_binary "$tmp" "$dest" ""; then
                installed=1
                # Ensure ~/.local/bin is on PATH for this session
                case ":$PATH:" in
                    *:"$HOME/.local/bin":*) ;;
                    *) export PATH="$HOME/.local/bin:$PATH" ;;
                esac
                echo "OK: marksman installed to $dest (~/.local/bin added to PATH)"
            fi
        else
            echo "OK: marksman installed to $dest"
        fi
        rm -f "$tmp"
        if [[ "$installed" -ne 1 ]]; then
            echo "ERROR: Could not install marksman to /usr/local/bin or ~/.local/bin."
            echo "Install manually: https://github.com/artempyanykh/marksman/releases"
            exit 1
        fi
    else
        echo "ERROR: Neither brew nor curl found."
        echo "Install manually: https://github.com/artempyanykh/marksman/releases"
        exit 1
    fi
}

# ── Phase 2: Verify ──────────────────────────────────────────────────
verify() {
    echo "[2/3] Verifying marksman is on PATH..."
    if command -v "$BINARY" &>/dev/null; then
        echo "OK: marksman found at $(command -v "$BINARY")"
        return 0
    fi

    # Search common locations
    for d in "$HOME/.local/bin" "/usr/local/bin" "/usr/bin"; do
        local candidate="$d/$BINARY"
        if [[ -x "$candidate" ]]; then
            echo "Found marksman at $candidate — adding $d to PATH..."
            case ":$PATH:" in
                *:"$d":*) ;;
                *) export PATH="$PATH:$d" ;;
            esac
            if command -v "$BINARY" &>/dev/null; then
                echo "OK: marksman found at $(command -v "$BINARY")"
                return 0
            fi
        fi
    done

    echo "ERROR: marksman not found on PATH after install"
    exit 1
}

# ── Phase 3: Health Check ────────────────────────────────────────────
# marksman defaults to stdio mode; no --stdio flag needed.
health_check() {
    echo "[3/3] Health check: testing stdio handshake..."
    local init_msg
    init_msg='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{},"rootUri":"file:///tmp"}}'
    local header
    header="Content-Length: ${#init_msg}\r\n\r\n"

    local response
    # Hold stdin open briefly after the request: marksman exits on EOF, so a
    # bare pipe gets closed before it has written the response.
    response=$({ printf "${header}${init_msg}"; sleep 3; } | _timeout 10 "$BINARY" 2>/dev/null | head -c 4096 || true)

    if [[ -n "$response" && "$response" == *"Content-Length"* ]]; then
        echo "OK: marksman responds to LSP initialize (stdio mode)"
    else
        echo "WARN: marksman did not respond to handshake (may need workspace context)"
    fi
}

# ── Main ──────────────────────────────────────────────────────────────
echo "=== ACowork LSP Setup: marksman (Markdown) ==="
install
verify
health_check
echo "=== Done ==="
