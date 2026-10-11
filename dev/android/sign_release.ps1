# sign_release.ps1 — One-stop Android release signing + APK build for ACowork Mobile.
#
# Flow:
#   1. Locate keytool (PATH / JAVA_HOME / common JDK & Android Studio JBR).
#   2. Choose the keystore file location (default: $HOME/acowork-mobile.keystore).
#   3. Create the keystore with keytool if needed (prompts for passwords + DN info).
#   4. Choose the plaintext gradle.properties location
#      (default: $HOME/.gradle/gradle.properties) and write the four
#      acowork.release.* keys. build.gradle.kts reads these via findProperty.
#   5. Optionally run `npx tauri android build --apk` (release is the default).
#
# The passwords are typed by the operator and land only in the local
# gradle.properties (inherent to Gradle signing). They are never printed and
# never committed. Reuses the empty-safe signing block in
# apps/acowork-mobile/src-tauri/gen/android/app/build.gradle.kts.

[CmdletBinding()]
param(
    # Non-interactive overrides. When omitted, the script prompts (Enter = default).
    [string]$StorePath,
    [string]$GradlePropsPath,
    [string]$KeytoolPath,
    [string]$Alias = "acowork",
    [int]$KeySize = 2048,
    [int]$Validity = 10000,
    [switch]$Build,          # build the release APK without prompting
    [switch]$NoBuild         # skip the build step entirely
)

$ErrorActionPreference = "Stop"

# ── Repo layout (script lives at dev/android/, root is two levels up) ────────
$ScriptDir  = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot   = Split-Path -Parent (Split-Path -Parent $ScriptDir)
$AppDir     = Join-Path $RepoRoot "apps\acowork-mobile"

# ── Defaults (the addresses used in the manual flow) ─────────────────────────
$defStore   = Join-Path $HOME "acowork-mobile.keystore"
$defGradle  = Join-Path $HOME ".gradle\gradle.properties"

function Unprotect-Secure([System.Security.SecureString]$s) {
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($s)
    try { return [Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

function Resolve-PathInteractive([string]$label, [string]$def, [string]$provided) {
    if ($provided) { return (Expand-PathProvided $provided) }
    $ans = Read-Host "$label`n  [默认: $def]（直接回车用默认，否则输入完整路径）"
    if ([string]::IsNullOrWhiteSpace($ans)) { return $def }
    return (Expand-PathProvided $ans)
}

function Expand-PathProvided([string]$p) {
    if ($p -like '~*') { $p = $HOME + $p.Substring(1) }
    return [IO.Path]::GetFullPath($p)
}

function Find-Keytool {
    $c = Get-Command keytool -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    if ($env:JAVA_HOME) {
        $p = Join-Path $env:JAVA_HOME "bin\keytool.exe"
        if (Test-Path $p) { return $p }
    }
    $roots = @(
        "$env:ProgramFiles\JetBrains",
        "${env:ProgramFiles(x86)}\JetBrains",
        "$env:ProgramFiles\Android\Android Studio\jbr",
        "$env:LOCALAPPDATA\Programs\Android Studio\jbr",
        "$env:ProgramFiles\Java",
        "$env:ProgramFiles\Eclipse Adoptium",
        "$env:ProgramFiles\Microsoft",
        "$env:ProgramFiles\Zulu",
        "$env:ProgramFiles\Amazon Corretto"
    )
    foreach ($r in $roots) {
        if (Test-Path $r) {
            $f = Get-ChildItem -Path $r -Recurse -Filter keytool.exe -Depth 4 -ErrorAction SilentlyContinue |
                 Select-Object -First 1
            if ($f) { return $f.FullName }
        }
    }
    return $null
}

Write-Host "=== ACowork Mobile — Android release signing ===" -ForegroundColor Cyan

# ── 1. keytool ────────────────────────────────────────────────────────────────
if (-not $KeytoolPath) {
    $KeytoolPath = Find-Keytool
    if ($KeytoolPath) { Write-Host "[keytool] 自动定位: $KeytoolPath" -ForegroundColor DarkGray }
}
if (-not $KeytoolPath) {
    $KeytoolPath = Read-Host "[keytool] 未自动找到，请输入 keytool.exe 的完整路径"
}
if (-not (Test-Path $KeytoolPath)) { throw "keytool 不存在: $KeytoolPath" }

# ── 2. keystore path ──────────────────────────────────────────────────────────
$StorePath = Resolve-PathInteractive "[keystore] 密钥库文件路径" $defStore $StorePath

# ── 3. create keystore (or reuse) ─────────────────────────────────────────────
$create = $true
if (Test-Path $StorePath) {
    Write-Host "[keystore] 已存在: $StorePath" -ForegroundColor Yellow
    $ans = Read-Host "  [1] 复用现有 keystore   [2] 删除并重建（输入 1 或 2，默认 1）"
    if ($ans -eq '2') {
        Remove-Item $StorePath -Force
        $create = $true
    } else {
        $create = $false
    }
}

if ($create) {
    Write-Host "[keystore] 创建新密钥库（口令输入不会回显）" -ForegroundColor Cyan
} else {
    Write-Host "[keystore] 复用现有密钥库，仅需口令以写入 gradle.properties" -ForegroundColor Cyan
}

$spPrompt = if ($create) { "设置 keystore 口令" } else { "输入 keystore 口令" }
$kpPrompt = if ($create) { "设置 key 口令（回车=与库口令相同）" } else { "输入 key 口令（回车=与库口令相同）" }
$sp = Read-Host $spPrompt -AsSecureString
$kp = Read-Host $kpPrompt -AsSecureString
$spP = Unprotect-Secure $sp
$kpP = if ($kp.Length) { Unprotect-Secure $kp } else { $spP }

if ($create) {
    # Build an X.500 DN; CN is required, the rest optional.
    $cn = Read-Host "  CN 姓名/通用名 [默认: ACowork Developer]"
    if ([string]::IsNullOrWhiteSpace($cn)) { $cn = "ACowork Developer" }
    $ou = Read-Host "  OU 部门（可留空）"
    $o  = Read-Host "  O 组织（可留空）"
    $l  = Read-Host "  L 城市（可留空）"
    $st = Read-Host "  ST 省份（可留空）"
    $cc = Read-Host "  C 国家两位码（可留空，如 CN）"
    $parts = [System.Collections.Generic.List[string]]::new()
    $parts.Add("CN=$cn")
    if ($ou) { $parts.Add("OU=$ou") }
    if ($o)  { $parts.Add("O=$o") }
    if ($l)  { $parts.Add("L=$l") }
    if ($st) { $parts.Add("ST=$st") }
    if ($cc) { $parts.Add("C=$cc") }
    $dname = $parts -join ","

    Write-Host "[keytool] 生成密钥对: $StorePath (alias=$Alias)" -ForegroundColor Cyan
    & $KeytoolPath -genkeypair -v -keystore $StorePath -alias $Alias `
        -keyalg RSA -keysize $KeySize -validity $Validity `
        -storepass $spP -keypass $kpP -dname $dname
    if ($LASTEXITCODE -ne 0) { throw "keytool 生成失败 (exit $LASTEXITCODE)" }
    Write-Host "[keytool] 完成。" -ForegroundColor Green
}

# ── 4. gradle.properties ──────────────────────────────────────────────────────
$GradlePropsPath = Resolve-PathInteractive "[gradle] 明文存储 gradle.properties 路径" $defGradle $GradlePropsPath

# Warn if the chosen location sits inside the repo — it holds plaintext passwords.
$normProps = [IO.Path]::GetFullPath($GradlePropsPath)
if ($normProps.StartsWith($RepoRoot, [StringComparison]::OrdinalIgnoreCase)) {
    Write-Host "[警告] gradle.properties 位于仓库内 ($RepoRoot)，含明文口令，务必不要提交！" -ForegroundColor Red
    if ($PSCmdlet.ShouldContinue("确认继续写入仓库内的 gradle.properties？", "位置不安全")) {
        # proceed
    } else {
        throw "已取消。请改选仓库外的路径（如 `$HOME\.gradle\gradle.properties）。"
    }
}

$propsDir = Split-Path -Parent $normProps
if ($propsDir -and -not (Test-Path $propsDir)) { New-Item -ItemType Directory -Force -Path $propsDir | Out-Null }

$keyNames = @('acowork.release.storeFile','acowork.release.storePassword','acowork.release.keyAlias','acowork.release.keyPassword')
$existing = @()
if (Test-Path $normProps) { $existing = @(Get-Content $normProps) }
# Keep unrelated/existing lines; drop the four we manage.
$kept = $existing | Where-Object {
    $idx = $_.IndexOf('=')
    if ($idx -gt 0) { $name = $_.Substring(0, $idx).Trim(); ($keyNames -notcontains $name) } else { $true }
}
# properties files treat backslash as an escape char — store the path with forward slashes.
$storeFwd = $StorePath -replace '\\','/'
$newLines = @(
    "acowork.release.storeFile=$storeFwd",
    "acowork.release.storePassword=$spP",
    "acowork.release.keyAlias=$Alias",
    "acowork.release.keyPassword=$kpP"
)
$text = (($kept + $newLines) -join "`n") + "`n"
[IO.File]::WriteAllText($normProps, $text, (New-Object Text.UTF8Encoding($false)))
Write-Host "[gradle] 已写入 $normProps（storeFile=$storeFwd, keyAlias=$Alias；口令为明文）" -ForegroundColor Green

# ── 5. build ──────────────────────────────────────────────────────────────────
$doBuild = $false
if ($Build)    { $doBuild = $true }
elseif ($NoBuild) { $doBuild = $false }
else {
    $ans = Read-Host "现在打 release APK 吗？(Y/n)"
    if ($ans -eq '' -or $ans -match '^[Yy]') { $doBuild = $true }
}

if ($doBuild) {
    # Windows: the Tauri CLI symlinks the .so into jniLibs. Non-admin processes need
    # Developer Mode for that; probe it BEFORE building so a full compile isn't wasted
    # only to fail at the symlink step.
    if ($IsWindows -or ($env:OS -match 'Windows')) {
        $probe    = Join-Path $env:TEMP ("acowork_symlink_probe_" + [guid]::NewGuid().ToString('N').Substring(0,8))
        $probeSrc = "$probe.txt"; $probeLnk = "$probe.lnk"
        function Test-SymOk {
            Set-Content -Path $probeSrc -Value "x" -Encoding ascii -EA SilentlyContinue
            $ok = $true
            try { New-Item -ItemType SymbolicLink -Path $probeLnk -Target $probeSrc -EA Stop | Out-Null }
            catch { $ok = $false }
            Remove-Item $probeSrc, $probeLnk -Force -EA SilentlyContinue
            return $ok
        }
        if (-not (Test-SymOk)) {
            Write-Host "[warn] 当前进程无法创建符号链接——Tauri CLI 会把 .so 软链进 jniLibs，否则构建会在该步中断。" -ForegroundColor Yellow
            Write-Host "       需开启 Windows 开发者模式（一次性，会弹 UAC）。现在开启吗？(Y/n)" -ForegroundColor Yellow
            $ans = Read-Host
            if ($ans -eq '' -or $ans -match '^[Yy]') {
                try {
                    Start-Process pwsh -Verb RunAs -Wait -ArgumentList '-NoProfile','-Command',
                        'reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock" /t REG_DWORD /f /v AllowDevelopmentWithoutDevLicense /d 1'
                } catch { Write-Host "[warn] 提权被取消，开发者模式未开启。" -ForegroundColor Yellow }
            }
            if (Test-SymOk) { Write-Host "[ok] 符号链接权限已就绪，继续构建。" -ForegroundColor Green }
            else { Write-Host "[warn] 仍无法创建符号链接。请以管理员终端重试，或按 README 用复制 .so + gradlew 绕过 CLI。" -ForegroundColor Yellow }
        }
    }
    Push-Location $AppDir
    try {
        Write-Host "[build] npx tauri android build --apk --target aarch64 (release is default)" -ForegroundColor Cyan
        npx tauri android build --apk --target aarch64
        if ($LASTEXITCODE -ne 0) { throw "构建失败 (exit $LASTEXITCODE)" }
        Write-Host "[build] 完成。产物：" -ForegroundColor Green
        Get-ChildItem -Path (Join-Path $AppDir "src-tauri\gen\android\app\build\outputs\apk") -Recurse -Filter *.apk -ErrorAction SilentlyContinue |
            ForEach-Object { Write-Host "  $($_.FullName)" -ForegroundColor Green }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "已跳过构建。之后可手动运行：cd apps/acowork-mobile && npx tauri android build --apk --target aarch64" -ForegroundColor DarkGray
}
