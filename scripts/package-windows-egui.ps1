# O2.6 - build, sign, audit and verify the Windows installer for the egui
# shell (EPIC O #209; spec 16.5, 19 Phase 9).
#
# The Tauri bundler produced the MSI for apps/desktop. The egui build has no
# bundler, so this script drives NSIS directly over
# apps/plaudon-egui/packaging/windows/plaudon-egui.nsi.
#
# Exactly the same honesty rule as the macOS script: an unsigned or
# signature-mismatched artifact is reported as `not_verified`, the release
# gate fails, and the script exits 3 - "built, NOT publishable". Nothing here
# prints "signed" for an unsigned binary.
#
#   pwsh scripts/package-windows-egui.ps1 [-Out <dir>] [-Evidence <dir>]
#                                         [-Features <cargo features>] [-SkipBuild]
#
# Environment:
#   PLAUDON_WINDOWS_CERT_PATH      PFX file holding the Authenticode certificate
#   PLAUDON_WINDOWS_CERT_PASSWORD  password for that file
#   PLAUDON_WINDOWS_TIMESTAMP_URL  RFC 3161 timestamp server (default: DigiCert)
#   PLAUDON_NSIS                   makensis.exe, if it is not in the default location
#   CARGO_BUILD_JOBS               cargo parallelism, default 2 (workstation policy)
#
# Reason tokens, shared with the macOS script and scripts/sign-cli.sh:
# unsupported_platform, missing_binary, missing_tool, signature_invalid,
# missing_credential, entitlement_rejected.

[CmdletBinding()]
param(
    [string]$Out = "dist/windows-egui",
    [string]$Evidence = "",
    [string]$Features = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$RepoRoot = (Get-Location).Path

if (-not $IsWindows) {
    Write-Error "reason=unsupported_platform: the Windows installer builds on Windows only (spec 18.4 forbids a Wine substitute)"
    exit 1
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$OutFull = (Resolve-Path $Out).Path
$Packaging = Join-Path $RepoRoot "apps/plaudon-egui/packaging/windows"
$Nsi = Join-Path $Packaging "plaudon-egui.nsi"
$Manifest = Join-Path $Packaging "plaudon-egui.exe.manifest"

# ------------------------------------------------------------------ build --
$Jobs = if ($env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS } else { "2" }
if (-not $SkipBuild) {
    Write-Host "==> building plaudon-egui and the automation CLI (release, -j $Jobs)"
    $BuildArgs = @("build", "--release", "-j", $Jobs, "-p", "plaudon-egui")
    if ($Features) { $BuildArgs += @("--features", $Features) }
    & cargo @BuildArgs
    if ($LASTEXITCODE -ne 0) { Write-Error "reason=missing_binary: cargo build failed"; exit 1 }
    & cargo build --release -j $Jobs -p plaudon-automation --bin plaudon
    if ($LASTEXITCODE -ne 0) { Write-Error "reason=missing_binary: the automation CLI failed to build"; exit 1 }
}

$Exe = Join-Path $RepoRoot "target/release/plaudon-egui.exe"
$Cli = Join-Path $RepoRoot "target/release/plaudon.exe"
foreach ($Required in @($Exe, $Cli)) {
    if (-not (Test-Path $Required)) {
        Write-Error "reason=missing_binary: $Required was not produced"
        exit 1
    }
}

# ------------------------------------------------- resource and privilege --
# The icon, the version block and the side-by-side manifest are linked in by
# apps/plaudon-egui/build.rs, which warns rather than fails when no resource
# compiler is installed. This is where that becomes an error: an executable
# with no version block lost its resources, and an installer must not ship it.
$FileVersion = (Get-Item $Exe).VersionInfo.FileVersion
if ([string]::IsNullOrWhiteSpace($FileVersion)) {
    Write-Error "reason=missing_binary: plaudon-egui.exe carries no version resource; install llvm-rc or the MSVC resource compiler and rebuild"
    exit 1
}
& findstr.exe /M /C:"longPathAware" $Exe > $null
if ($LASTEXITCODE -ne 0) {
    Write-Error "reason=missing_binary: the side-by-side manifest is not embedded in plaudon-egui.exe"
    exit 1
}
Write-Host "==> embedded version resource: $FileVersion"

# The privilege audit the Tauri script did by dumping capability manifests.
# This build has no capability manifest; what it has is a requested execution
# level and a registry footprint, and both are asserted here.
$ManifestText = Get-Content $Manifest -Raw
if ($ManifestText -notmatch 'level="asInvoker"' -or $ManifestText -match 'requireAdministrator' -or $ManifestText -match 'highestAvailable') {
    Write-Error "reason=entitlement_rejected: the application manifest must request asInvoker and nothing higher (spec 16.5)"
    exit 1
}
if ($ManifestText -match 'uiAccess="true"') {
    Write-Error "reason=entitlement_rejected: uiAccess is not a privilege this product may ship"
    exit 1
}
$NsiText = Get-Content $Nsi -Raw
if ($NsiText -match 'HKLM' -or $NsiText -match 'HKEY_LOCAL_MACHINE') {
    Write-Error "reason=entitlement_rejected: the installer is per-user; it may not write to HKLM"
    exit 1
}
if ($NsiText -notmatch 'RequestExecutionLevel user') {
    Write-Error "reason=entitlement_rejected: the installer must run as the invoking user"
    exit 1
}

# ------------------------------------------------------------------ stage --
$Stage = Join-Path $OutFull "stage"
if (Test-Path $Stage) { Remove-Item $Stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe $Stage -Force
Copy-Item $Cli $Stage -Force

$Version = (Select-String -Path (Join-Path $RepoRoot "Cargo.toml") -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $Version) { Write-Error "reason=missing_binary: the workspace version could not be read"; exit 1 }

# --------------------------------------------------------------- signing ---
# The payload is signed before it is packed, so what the installer writes to
# disk is signed; the installer itself is signed afterwards. NSIS generates
# the uninstaller at install time, so it cannot be signed here - the report
# records that rather than glossing over it.
$SignStatus = "not_verified"
$SignDetail = "no Authenticode certificate supplied"
$SignTool = $null
$CertPath = $env:PLAUDON_WINDOWS_CERT_PATH

function Invoke-Sign([string]$Target) {
    $TimestampUrl = if ($env:PLAUDON_WINDOWS_TIMESTAMP_URL) { $env:PLAUDON_WINDOWS_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
    & $SignTool.FullName sign /fd SHA256 /tr $TimestampUrl /td SHA256 `
        /f $CertPath /p $env:PLAUDON_WINDOWS_CERT_PASSWORD $Target
    if ($LASTEXITCODE -ne 0) {
        Write-Error "reason=signature_invalid: signing failed for $Target"
        exit 1
    }
    $Signature = Get-AuthenticodeSignature $Target
    if ($Signature.Status -ne "Valid") {
        Write-Error "reason=signature_invalid: $Target reports $($Signature.Status)"
        exit 1
    }
}

if ($CertPath -and (Test-Path $CertPath)) {
    $SignTool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "x64" } | Select-Object -First 1
    if (-not $SignTool) {
        Write-Error "reason=missing_credential: signtool.exe was not found"
        exit 1
    }
    Write-Host "==> signing the payload"
    Invoke-Sign (Join-Path $Stage "plaudon-egui.exe")
    Invoke-Sign (Join-Path $Stage "plaudon.exe")
} elseif ($CertPath) {
    Write-Error "reason=missing_credential: PLAUDON_WINDOWS_CERT_PATH is set to '$CertPath', which does not exist"
    exit 1
} else {
    Write-Host "==> no Authenticode certificate; the artifact will be unsigned and NOT publishable"
}

# ------------------------------------------------------------- installer ---
$MakeNsis = $env:PLAUDON_NSIS
if (-not $MakeNsis) {
    $Candidates = @(
        "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
        "${env:ProgramFiles}\NSIS\makensis.exe"
    ) | Where-Object { Test-Path $_ }
    $MakeNsis = $Candidates | Select-Object -First 1
}
if (-not $MakeNsis) {
    $Found = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($Found) { $MakeNsis = $Found.Source }
}
if (-not $MakeNsis) {
    Write-Error "reason=missing_tool: makensis.exe was not found; install NSIS 3 (choco install nsis) or set PLAUDON_NSIS"
    exit 1
}

$Installer = Join-Path $OutFull "Plaudon-$Version-x64-setup.exe"
if (Test-Path $Installer) { Remove-Item $Installer -Force }
Write-Host "==> building the installer with $MakeNsis"
& $MakeNsis /WX "/DPLAUDON_VERSION=$Version" "/DPLAUDON_STAGE=$Stage" "/DPLAUDON_OUTFILE=$Installer" $Nsi
if ($LASTEXITCODE -ne 0) {
    Write-Error "reason=missing_binary: makensis failed"
    exit 1
}
if (-not (Test-Path $Installer)) {
    Write-Error "reason=missing_binary: no installer was produced"
    exit 1
}

if ($SignTool) {
    Write-Host "==> signing the installer"
    Invoke-Sign $Installer
    $SignStatus = "verified"
    $SignDetail = "Authenticode signature verified on the installer and both installed executables; the NSIS uninstaller is generated at install time and is not signed"
}

# ---------------------------------------------------------------- report ---
$Sha = (Get-FileHash $Installer -Algorithm SHA256).Hash.ToLower()
$ExeSha = (Get-FileHash (Join-Path $Stage "plaudon-egui.exe") -Algorithm SHA256).Hash.ToLower()
$CliSha = (Get-FileHash (Join-Path $Stage "plaudon.exe") -Algorithm SHA256).Hash.ToLower()

$Report = [ordered]@{
    platform             = "windows"
    ui                   = "egui"
    artifact             = (Split-Path $Installer -Leaf)
    artifact_sha256      = $Sha
    executable_sha256    = $ExeSha
    cli_binary_sha256    = $CliSha
    installer_tool       = "NSIS ($MakeNsis)"
    install_scope        = "per-user: %LOCALAPPDATA%\Programs\Plaudon, HKCU only"
    data_directory       = "%APPDATA%\app.plaudon.desktop"
    file_associations    = ".plaudon via the Plaudon.Archive ProgID; plaudon:// URL scheme"
    embedded_version     = $FileVersion
    signature_status     = $SignStatus
    signature_detail     = $SignDetail
    notarization_status  = "not_applicable"
    privilege_audit      = "manifest requests asInvoker; the installer writes HKCU only"
    host_os_build        = (Get-CimInstance Win32_OperatingSystem).BuildNumber
    commit               = (git rev-parse HEAD)
    publishable          = ($SignStatus -eq "verified")
}
$ReportPath = Join-Path $OutFull "windows-signing-report.json"
$Report | ConvertTo-Json -Depth 4 | Set-Content -Path $ReportPath -Encoding utf8

if ($Evidence) {
    New-Item -ItemType Directory -Force -Path (Join-Path $Evidence "signing-verification") | Out-Null
    Copy-Item $ReportPath (Join-Path $Evidence "signing-verification/windows-egui.json") -Force
}

Write-Host "artifact: $Installer"
Write-Host "sha256:   $Sha"
Write-Host "signature: $SignStatus ($SignDetail)"

if ($SignStatus -ne "verified") {
    Write-Error "reason=missing_credential: the installer is built but NOT publishable"
    exit 3
}
