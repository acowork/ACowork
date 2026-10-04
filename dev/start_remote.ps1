# Start the full stack with the Gateway reachable on the LAN (mobile testing).
# Wraps dev/build_core.ps1 -Start -Remote and adds the Windows Defender
# Firewall allow rule the Remote mode itself does not manage.
# Needs an elevated shell for the firewall rules. -Debug builds the debug
# profile (faster compile, verbose logs).
[CmdletBinding()]
param([switch]$Debug)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# Inbound allow for the HTTP + MQTT ports (Private profile only).
foreach ($port in 19876, 19875) {
    $name = "ACowork LAN :$port"
    if (-not (Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow `
            -Protocol TCP -LocalPort $port -Profile Private | Out-Null
        Write-Host "Firewall: allowed inbound TCP $port (Private profile)" -ForegroundColor Green
    } else {
        Write-Host "Firewall: rule already present for TCP $port" -ForegroundColor DarkGray
    }
}

$build = Join-Path $root "dev/build_core.ps1"
$flags = @("-Start", "-Remote")
if ($Debug) { $flags += "-Debug" }
& $build @flags

$ip = & (Join-Path $root "dev/lan_ip.ps1")
Write-Host ""
Write-Host "Mobile app: enter Gateway address  http://${ip}:19876" -ForegroundColor Cyan
