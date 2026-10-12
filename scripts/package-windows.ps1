# N2.1 - build, sign and verify the Windows installer (spec 16.5, 19 Phase 9).
#
# Exactly the same honesty rule as the macOS script: an unsigned or
# signature-mismatched artifact is reported as `not_verified` and the release
# gate fails. Nothing here prints "signed" for an unsigned binary.
#
#   pwsh scripts/package-windows.ps1 [-Out <dir>] [-Evidence <dir>]
#
# Environment:
#   PLAUDON_WINDOWS_CERT_PATH      PFX file holding the Authenticode certificate
#   PLAUDON_WINDOWS_CERT_PASSWORD  password for that file
#   PLAUDON_WINDOWS_TIMESTAMP_URL  RFC 3161 timestamp server (default: DigiCert)

[CmdletBinding()]
param(
    [string]$Out = "dist/windows",
    [string]$Evidence = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$RepoRoot = (Get-Location).Path

if (-not $IsWindows) {
    Write-Error "reason=unsupported_platform: the Windows installer builds on Windows only (spec 18.4 forbids a Wine substitute)"
    exit 1
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null

Write-Host "==> building the installer"
Push-Location apps/desktop
npm ci --no-audit --no-fund
npx tauri build --bundles msi
Pop-Location

$Msi = Get-ChildItem -Path (Join-Path $RepoRoot "target/release/bundle/msi") -Filter *.msi -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $Msi) {
    Write-Error "reason=missing_binary: no MSI installer was produced"
    exit 1
}
$Exe = Join-Path $RepoRoot "target/release/plaudon-desktop.exe"
$Cli = Join-Path $RepoRoot "target/release/plaudon.exe"

# Spec 16.5: production bundles must not enable devtools.
$Config = Get-Content (Join-Path $RepoRoot "apps/desktop/src-tauri/tauri.conf.json") -Raw
if ($Config -match '"devtools"\s*:\s*true') {
    Write-Error "reason=devtools_enabled: production bundles must have devtools disabled (spec 16.5)"
    exit 1
}

Write-Host "==> effective Tauri capabilities"
Get-ChildItem (Join-Path $RepoRoot "apps/desktop/src-tauri/capabilities") -Filter *.json |
    ForEach-Object { Get-Content $_.FullName -Raw | Write-Host }

$SignStatus = "not_verified"
$SignDetail = "no Authenticode certificate supplied"

$CertPath = $env:PLAUDON_WINDOWS_CERT_PATH
if ($CertPath -and (Test-Path $CertPath)) {
    $TimestampUrl = if ($env:PLAUDON_WINDOWS_TIMESTAMP_URL) { $env:PLAUDON_WINDOWS_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
    $SignTool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "x64" } | Select-Object -First 1
    if (-not $SignTool) {
        Write-Error "reason=missing_credential: signtool.exe was not found"
        exit 1
    }
    foreach ($Target in @($Exe, $Cli, $Msi.FullName)) {
        if (Test-Path $Target) {
            & $SignTool.FullName sign /fd SHA256 /tr $TimestampUrl /td SHA256 `
                /f $CertPath /p $env:PLAUDON_WINDOWS_CERT_PASSWORD $Target
            if ($LASTEXITCODE -ne 0) {
                Write-Error "reason=signature_invalid: signing failed for $Target"
                exit 1
            }
        }
    }
    foreach ($Target in @($Exe, $Cli, $Msi.FullName)) {
        if (Test-Path $Target) {
            $Signature = Get-AuthenticodeSignature $Target
            if ($Signature.Status -ne "Valid") {
                Write-Error "reason=signature_invalid: $Target reports $($Signature.Status)"
                exit 1
            }
        }
    }
    $SignStatus = "verified"
    $SignDetail = "Authenticode signature verified on the installer and both executables"
} else {
    Write-Host "==> no Authenticode certificate; the artifact is unsigned and NOT publishable"
}

$Sha = (Get-FileHash $Msi.FullName -Algorithm SHA256).Hash.ToLower()
$CliSha = $null
if (Test-Path $Cli) { $CliSha = (Get-FileHash $Cli -Algorithm SHA256).Hash.ToLower() }

$Report = [ordered]@{
    platform             = "windows"
    artifact             = $Msi.Name
    artifact_sha256      = $Sha
    cli_binary_sha256    = $CliSha
    signature_status     = $SignStatus
    signature_detail     = $SignDetail
    notarization_status  = "not_applicable"
    devtools_enabled     = $false
    host_os_build        = (Get-CimInstance Win32_OperatingSystem).BuildNumber
    commit               = (git rev-parse HEAD)
    publishable          = ($SignStatus -eq "verified")
}
$ReportPath = Join-Path $Out "windows-signing-report.json"
$Report | ConvertTo-Json -Depth 4 | Set-Content -Path $ReportPath -Encoding utf8

if ($Evidence) {
    New-Item -ItemType Directory -Force -Path (Join-Path $Evidence "signing-verification") | Out-Null
    Copy-Item $ReportPath (Join-Path $Evidence "signing-verification/windows.json") -Force
}

Copy-Item $Msi.FullName $Out -Force
Write-Host "artifact: $($Msi.Name)"
Write-Host "sha256:   $Sha"
Write-Host "signature: $SignStatus ($SignDetail)"

if ($SignStatus -ne "verified") {
    Write-Error "reason=missing_credential: the installer is built but NOT publishable"
    exit 3
}
