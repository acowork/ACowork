#!/usr/bin/env bash
# gen_icons.sh - Regenerate app icons for BOTH desktop and mobile from one master source.
# Usage: ./dev/common/gen_icons.sh [SOURCE_ICON] [--no-sync-res]
#   SOURCE_ICON    squared PNG/SVG master. default: <repo>/assets/app-icon.svg
#   --no-sync-res  skip syncing mobile android icons into the checked-out gen res
#   -h, --help     show this help
#
# Why: `tauri icon` has no per-platform switch -- one run writes the FULL set
# (desktop + iOS + Android) into a single output dir. That is exactly why mobile
# icons used to pollute the desktop app. This script encodes the correct split:
#   1. mobile : tauri icon -> src-tauri/icons (KEEP android/ + ios/), then DELETE the
#             Windows/macOS desktop-only artifacts tauri also dumped there
#             (Store logos / *.icns / *.ico / icon.png / 64x64 / 128x128@2x)
#   2. desktop: tauri icon -> src-tauri/icons, then DELETE icons/android + icons/ios
#             (desktop bundle.icon never references them)
#   3. mobile : (optional) sync icons/android/* into gen/android res so the next
#             `tauri android build` picks them up WITHOUT re-running
#             `tauri android init` (which would clobber the hand-written signing block).
# Supports: Linux, macOS, Windows (Git Bash, WSL, MSYS2)

set -e

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; GRAY='\033[0;37m'; NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
MOBILE_DIR="$WORKSPACE_ROOT/apps/acowork-mobile"
DESKTOP_DIR="$WORKSPACE_ROOT/apps/acowork-desktop"

SOURCE=""
SYNC_RES=true
for arg in "$@"; do
    case "$arg" in
        --no-sync-res) SYNC_RES=false ;;
        -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) SOURCE="$arg" ;;
    esac
done
[ -z "$SOURCE" ] && SOURCE="$WORKSPACE_ROOT/assets/app-icon.svg"
case "$SOURCE" in
    /*) : ;;                       # already absolute
    *) SOURCE="$(cd "$(dirname "$SOURCE")" && pwd)/$(basename "$SOURCE")" ;;
esac
if [ ! -f "$SOURCE" ]; then echo -e "${RED}源图标不存在: $SOURCE${NC}"; exit 1; fi

gen_icons() {
    local app_dir="$1"
    echo -e "${GRAY}  -> $app_dir${NC}"
    ( cd "$app_dir" && npx tauri icon "$SOURCE" -o src-tauri/icons )
}

echo -e "${CYAN}=== ACowork — 生成 desktop + mobile 图标 ===${NC}"
echo -e "${GRAY}[src] $SOURCE${NC}"

# 1. mobile: generate, keep android/ + ios/, drop desktop-only artifacts
echo -e "${YELLOW}[mobile] tauri icon（保留 android/ios，删桌面图标）${NC}"
gen_icons "$MOBILE_DIR"
MOBILE_ICONS="$MOBILE_DIR/src-tauri/icons"
# mobile's bundle.icon lists 256/512 but tauri icon never emits those sizes (they
# were left as flat-blue scaffold placeholders). Derive them from this run's real art:
# 128x128@2x.png IS 256px, icon.png IS 512px. Do this BEFORE deleting the desktop junk.
for pair in "128x128@2x.png:256x256.png" "icon.png:512x512.png"; do
    from="${pair%%:*}"; to="${pair##*:}"
    if [ -f "$MOBILE_ICONS/$from" ]; then cp -f "$MOBILE_ICONS/$from" "$MOBILE_ICONS/$to"; echo -e "${GRAY}  [derive] $to <= $from${NC}"; fi
done
# tauri icon has no per-platform switch, so it also emits Windows/macOS-only files
# into the mobile dir. A mobile app never bundles those -> remove them.
for j in icon.icns icon.ico StoreLogo.png icon.png 64x64.png 128x128@2x.png; do
    if [ -f "$MOBILE_ICONS/$j" ]; then rm -f "$MOBILE_ICONS/$j"; echo -e "${GRAY}  [clean] 删除 $j${NC}"; fi
done
for s in "$MOBILE_ICONS"/Square*Logo.png; do
    [ -f "$s" ] || continue
    rm -f "$s"; echo -e "${GRAY}  [clean] 删除 $(basename "$s")${NC}"
done

# Android adaptive icons: tauri icon renders the mark full-bleed, but the launcher
# masks to a circle/squircle and only guarantees the central 66dp -> the mark looks
# oversized and cropped corners show the default white bg. Inset the foreground into
# the safe zone and set a brand-colored background. (Needs Pillow; skipped if absent.)
ANDROID_DIR="$MOBILE_ICONS/android"
if [ -d "$ANDROID_DIR" ]; then
    PY="$(command -v python3 || command -v python || true)"
    if [ -n "$PY" ]; then
        "$PY" "$SCRIPT_DIR/gen_icons_android.py" "$ANDROID_DIR" || echo -e "${YELLOW}  [warn] Android 安全区后处理失败（Pillow 缺失？）${NC}"
    else
        echo -e "${YELLOW}  [warn] 未找到 python，跳过 Android 自适应图标安全区修正${NC}"
    fi
fi

# 2. desktop: generate, then remove the mobile-only dirs
echo -e "${YELLOW}[desktop] tauri icon（随后删除 android/ios）${NC}"
gen_icons "$DESKTOP_DIR"
for d in android ios; do
    p="$DESKTOP_DIR/src-tauri/icons/$d"
    if [ -d "$p" ]; then rm -rf "$p"; echo -e "${GRAY}  [clean] 删除 $d${NC}"; fi
done

# 3. mobile: sync android icons into a checked-out gen res (no android init)
ICONS_ANDROID="$MOBILE_DIR/src-tauri/icons/android"
GEN_RES="$MOBILE_DIR/src-tauri/gen/android/app/src/main/res"
if [ "$SYNC_RES" = "true" ]; then
    if [ -d "$ICONS_ANDROID" ] && [ -d "$GEN_RES" ]; then
        echo -e "${CYAN}[sync] icons/android -> gen res (mipmap-* + values/ic_launcher_background.xml)${NC}"
        for m in "$ICONS_ANDROID"/mipmap-*; do
            [ -d "$m" ] || continue
            mkdir -p "$GEN_RES/$(basename "$m")"
            cp -rf "$m/." "$GEN_RES/$(basename "$m")/"
        done
        bg="$ICONS_ANDROID/values/ic_launcher_background.xml"
        if [ -f "$bg" ]; then mkdir -p "$GEN_RES/values"; cp -f "$bg" "$GEN_RES/values/"; fi
        echo -e "${GREEN}  [ok] 已同步；下次 'tauri android build' 生效（未动 build.gradle.kts，签名安全）${NC}"
    elif [ -d "$ICONS_ANDROID" ]; then
        echo -e "${GRAY}[sync] 跳过：gen/android 尚未生成（首次 'tauri android build' 会自动 init 并从 icons/android 取图标）。${NC}"
    fi
else
    echo -e "${GRAY}[sync] 已按 --no-sync-res 跳过 gen res 同步。${NC}"
fi

echo -e "${GREEN}=== 完成。desktop 已无移动图标，mobile 已含 android/ios ===${NC}"
