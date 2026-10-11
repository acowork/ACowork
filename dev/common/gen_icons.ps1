# gen_icons.ps1 — Regenerate app icons for BOTH desktop and mobile from one master source.
#
# Why this exists:
#   `tauri icon` has no per-platform switch — one run writes the FULL icon set
#   (desktop + iOS + Android) into a single output dir. That is exactly why the
#   mobile icons used to pollute the desktop app. This script encodes the correct
#   workflow so swapping the brand icon is a single command:
#
#   1. mobile : tauri icon -> src-tauri/icons (KEEP android/ + ios/), then DELETE the
#               Windows/macOS desktop-only artifacts tauri also dumped there
#               (Store logos / *.icns / *.ico / icon.png / 64x64 / 128x128@2x)
#   2. desktop: tauri icon -> src-tauri/icons, then DELETE icons/android + icons/ios
#               (the desktop bundle.icon never references them)
#   3. mobile : (optional) sync icons/android/* into the checked-out gen/android res
#               so the next `tauri android build` picks them up WITHOUT re-running
#               `tauri android init` (which would clobber the hand-written signing block).
#
# Usage:
#   ./dev/common/gen_icons.ps1 [-Source <path>] [-NoSyncRes]
#   default -Source = <repo>/assets/app-icon.svg

[CmdletBinding()]
param(
    [string]$Source,        # master icon (squared PNG/SVG). default: assets/app-icon.svg
    [switch]$NoSyncRes      # skip syncing mobile android icons into gen res
)

$ErrorActionPreference = "Stop"

# ── Repo layout (script lives at dev/common/, root is two levels up) ──────────
$ScriptDir  = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot   = Split-Path -Parent (Split-Path -Parent $ScriptDir)
$MobileDir  = Join-Path $RepoRoot "apps/acowork-mobile"
$DesktopDir = Join-Path $RepoRoot "apps/acowork-desktop"

if (-not $Source) { $Source = Join-Path $RepoRoot "assets/app-icon.svg" }
if (-not [IO.Path]::IsPathRooted($Source)) { $Source = [IO.Path]::GetFullPath((Join-Path (Get-Location) $Source)) }
if (-not (Test-Path $Source)) { throw "源图标不存在: $Source" }

function Invoke-IconGen([string]$appDir, [string]$src) {
    Push-Location $appDir
    try {
        # -o is relative to the app dir (next to its tauri.conf.json).
        npx tauri icon $src -o src-tauri/icons
        if ($LASTEXITCODE -ne 0) { throw "tauri icon 失败 (exit $LASTEXITCODE) in $appDir" }
    } finally { Pop-Location }
}

Write-Host "=== ACowork — 生成 desktop + mobile 图标 ===" -ForegroundColor Cyan
Write-Host "[src] $Source" -ForegroundColor DarkGray

# ── 1. mobile: generate, keep android/ + ios/, drop desktop-only artifacts ────
Write-Host "[mobile] tauri icon -> apps/acowork-mobile/src-tauri/icons (保留 android/ios，删桌面图标)" -ForegroundColor Cyan
Invoke-IconGen $MobileDir $Source
$mobileIcons = Join-Path $MobileDir "src-tauri/icons"
# mobile's bundle.icon lists 256/512 but `tauri icon` never emits those sizes (they
# were left as flat-blue scaffold placeholders). Derive them from this run's real art:
# 128x128@2x.png IS 256px, icon.png IS 512px. Do this BEFORE deleting the desktop junk.
foreach ($pair in @(@('128x128@2x.png','256x256.png'), @('icon.png','512x512.png'))) {
    $from = Join-Path $mobileIcons $pair[0]; $to = Join-Path $mobileIcons $pair[1]
    if (Test-Path $from) { Copy-Item $from $to -Force; Write-Host "  [derive] $($pair[1]) <= $($pair[0])" -ForegroundColor DarkGray }
}
# `tauri icon` has no per-platform switch, so it also emits Windows/macOS-only files
# into the mobile dir. A mobile app never bundles those -> remove them.
foreach ($j in @('icon.icns','icon.ico','StoreLogo.png','icon.png','64x64.png','128x128@2x.png')) {
    $p = Join-Path $mobileIcons $j
    if (Test-Path $p) { Remove-Item -Force $p; Write-Host "  [clean] 删除 $j" -ForegroundColor DarkGray }
}
Get-ChildItem $mobileIcons -Filter 'Square*Logo.png' -File -EA SilentlyContinue | ForEach-Object {
    Remove-Item -Force $_.FullName; Write-Host "  [clean] 删除 $($_.Name)" -ForegroundColor DarkGray
}

# Android adaptive icons: tauri icon renders the mark full-bleed, but the launcher
# masks to a circle/squircle and only guarantees the central 66dp -> the mark looks
# oversized and cropped corners show the default white bg. Inset the foreground into
# the safe zone and set a brand-colored background. (Needs Pillow; skipped if absent.)
$androidDir = Join-Path $mobileIcons "android"
if (Test-Path $androidDir) {
    $py = (Get-Command python -EA SilentlyContinue)
    if (-not $py) { $py = (Get-Command python3 -EA SilentlyContinue) }
    if ($py) {
        & $py.Source (Join-Path $ScriptDir "gen_icons_android.py") $androidDir
        if ($LASTEXITCODE -ne 0) { Write-Host "  [warn] Android 安全区后处理失败（Pillow 缺失？exit $LASTEXITCODE）" -ForegroundColor Yellow }
    } else {
        Write-Host "  [warn] 未找到 python，跳过 Android 自适应图标安全区修正" -ForegroundColor Yellow
    }
}

# ── 2. desktop: generate, then remove the mobile-only dirs ────────────────────
Write-Host "[desktop] tauri icon -> apps/acowork-desktop/src-tauri/icons (随后删除 android/ios)" -ForegroundColor Cyan
Invoke-IconGen $DesktopDir $Source
foreach ($d in @("android","ios")) {
    $p = Join-Path (Join-Path $DesktopDir "src-tauri/icons") $d
    if (Test-Path $p) { Remove-Item -Recurse -Force $p; Write-Host "  [clean] 删除 $d" -ForegroundColor DarkGray }
}

# ── 3. mobile: sync android icons into a checked-out gen res (no android init) ─
$iconsAndroid = Join-Path $MobileDir "src-tauri/icons/android"
$genRes       = Join-Path $MobileDir "src-tauri/gen/android/app/src/main/res"
if (-not $NoSyncRes) {
    if ((Test-Path $iconsAndroid) -and (Test-Path $genRes)) {
        Write-Host "[sync] icons/android -> gen res (mipmap-* + values/ic_launcher_background.xml)" -ForegroundColor Cyan
        Copy-Item -Path (Join-Path $iconsAndroid "mipmap-*") -Destination $genRes -Recurse -Force
        $bg = Join-Path $iconsAndroid "values/ic_launcher_background.xml"
        if (Test-Path $bg) { Copy-Item -Path $bg -Destination (Join-Path $genRes "values") -Force }
        Write-Host "  [ok] 已同步；下次 'tauri android build' 生效（未动 build.gradle.kts，签名安全）" -ForegroundColor Green
    } elseif (Test-Path $iconsAndroid) {
        Write-Host "[sync] 跳过：gen/android 尚未生成（首次构建时 'tauri android build' 会自动 init 并从 icons/android 取图标）。" -ForegroundColor DarkGray
    }
} else {
    Write-Host "[sync] 已按 -NoSyncRes 跳过 gen res 同步。" -ForegroundColor DarkGray
}

Write-Host "=== 完成。desktop 已无移动图标，mobile 已含 android/ios ===" -ForegroundColor Green
