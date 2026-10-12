# O2.6 - build, sign, audit and verify the Windows installer for the egui
# shell (EPIC O #209; spec 16.5, 19 Phase 9). Rewritten for WiX at O2.10
# (#250), when the deliverable became a per-user `.msi`.
#
# The Tauri bundler produced the MSI for apps/desktop. The egui build has no
# bundler, so this script drives the WiX toolset directly over
# apps/plaudon-egui/packaging/windows/plaudon-egui.wxs.
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
#   PLAUDON_WIX                    wix.exe, if it is not on PATH
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

# The pinned toolset. `wix build` binds the database through Windows'
# own msi.dll, so this is a Windows-only step however the .wxs is written;
# docs/EGUI_BUILD.md §6 says what a Linux host can and cannot check.
$WixVersion = "5.0.2"

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$OutFull = (Resolve-Path $Out).Path
$Packaging = Join-Path $RepoRoot "apps/plaudon-egui/packaging/windows"
$Wxs = Join-Path $Packaging "plaudon-egui.wxs"
$Manifest = Join-Path $Packaging "plaudon-egui.exe.manifest"
$Icon = Join-Path $Packaging "plaudon.ico"

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
# level and a registry footprint, and both are asserted here - first against
# the sources, and again below against the database that was actually built.
$ManifestText = Get-Content $Manifest -Raw
if ($ManifestText -notmatch 'level="asInvoker"' -or $ManifestText -match 'requireAdministrator' -or $ManifestText -match 'highestAvailable') {
    Write-Error "reason=entitlement_rejected: the application manifest must request asInvoker and nothing higher (spec 16.5)"
    exit 1
}
if ($ManifestText -match 'uiAccess="true"') {
    Write-Error "reason=entitlement_rejected: uiAccess is not a privilege this product may ship"
    exit 1
}
$WxsText = Get-Content $Wxs -Raw
if ($WxsText -match 'HKLM' -or $WxsText -match 'HKEY_LOCAL_MACHINE' -or $WxsText -match 'Root="HKMU"') {
    Write-Error "reason=entitlement_rejected: the installer is per-user; it may not write to HKLM"
    exit 1
}
if ($WxsText -notmatch 'Scope="perUser"') {
    Write-Error "reason=entitlement_rejected: the installer must run as the invoking user"
    exit 1
}

# ------------------------------------------------------------------ stage --
# The bind path WiX resolves every payload from: what is in here is what ends
# up in the package, which is also why the payload is signed in here rather
# than in target/.
$Stage = Join-Path $OutFull "stage"
if (Test-Path $Stage) { Remove-Item $Stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe $Stage -Force
Copy-Item $Cli $Stage -Force
Copy-Item $Icon $Stage -Force

$Version = (Select-String -Path (Join-Path $RepoRoot "Cargo.toml") -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $Version) { Write-Error "reason=missing_binary: the workspace version could not be read"; exit 1 }

# --------------------------------------------------------------- signing ---
# The payload is signed before it is packed, so what the installer writes to
# disk is signed; the package itself is signed afterwards. There is no
# generated uninstaller to sign: Windows Installer removes what this database
# installed, using its own code.
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
$Wix = $env:PLAUDON_WIX
if (-not $Wix) {
    $Found = Get-Command wix -ErrorAction SilentlyContinue
    if ($Found) { $Wix = $Found.Source }
}
if (-not $Wix) {
    $Candidate = Join-Path $env:USERPROFILE ".dotnet\tools\wix.exe"
    if (Test-Path $Candidate) { $Wix = $Candidate }
}
if (-not $Wix) {
    Write-Error "reason=missing_tool: wix.exe was not found; run ``dotnet tool install --global wix --version $WixVersion`` or set PLAUDON_WIX"
    exit 1
}
$WixReported = (& $Wix --version) -join ""
if (-not $WixReported.StartsWith($WixVersion)) {
    Write-Error "reason=missing_tool: this repository builds the installer with WiX $WixVersion, and $Wix reports '$WixReported'; a toolset upgrade is a reviewed change, not an accident"
    exit 1
}
Write-Host "==> building the installer with WiX $WixReported"

$Installer = Join-Path $OutFull "Plaudon-$Version-x64.msi"
if (Test-Path $Installer) { Remove-Item $Installer -Force }
& $Wix build -arch x64 -d "Version=$Version" -b $Stage -o $Installer $Wxs
if ($LASTEXITCODE -ne 0) {
    Write-Error "reason=missing_binary: wix build failed"
    exit 1
}
if (-not (Test-Path $Installer)) {
    Write-Error "reason=missing_binary: no installer was produced"
    exit 1
}

# --------------------------------------------------- audit of the database --
# The source audit above says what we wrote; this says what was built. The
# package is opened read-only through Windows Installer's own automation
# interface, so these are the rows the operating system will act on.
$WindowsInstaller = New-Object -ComObject WindowsInstaller.Installer
$Database = $WindowsInstaller.GetType().InvokeMember(
    "OpenDatabase", "InvokeMethod", $null, $WindowsInstaller, @($Installer, 0))

function Invoke-MsiQuery([string]$Sql) {
    $View = $Database.GetType().InvokeMember("OpenView", "InvokeMethod", $null, $Database, @($Sql))
    $View.GetType().InvokeMember("Execute", "InvokeMethod", $null, $View, $null) | Out-Null
    $Rows = @()
    while ($true) {
        $Record = $View.GetType().InvokeMember("Fetch", "InvokeMethod", $null, $View, $null)
        if (-not $Record) { break }
        $Count = $Record.GetType().InvokeMember("FieldCount", "GetProperty", $null, $Record, $null)
        $Row = @()
        for ($Field = 1; $Field -le $Count; $Field++) {
            $Row += $Record.GetType().InvokeMember("StringData", "GetProperty", $null, $Record, @($Field))
        }
        $Rows += , $Row
    }
    $View.GetType().InvokeMember("Close", "InvokeMethod", $null, $View, $null) | Out-Null
    return $Rows
}

# Registry root 2 is HKLM and -1 is "whichever hive the install scope picks",
# which for a package that must never be machine-wide is just as wrong.
$RegistryRows = Invoke-MsiQuery "SELECT ``Root``, ``Key`` FROM ``Registry``"
foreach ($Row in $RegistryRows) {
    if ($Row[0] -ne "1") {
        Write-Error "reason=entitlement_rejected: the built package writes registry root $($Row[0]) at '$($Row[1])'; a per-user install writes HKCU (1) and nothing else"
        exit 1
    }
}
$AllUsers = Invoke-MsiQuery "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='ALLUSERS'"
if ($AllUsers.Count -ne 0 -and $AllUsers[0][0] -ne "") {
    Write-Error "reason=entitlement_rejected: ALLUSERS is '$($AllUsers[0][0])'; a per-user install leaves it empty"
    exit 1
}
# Summary information property 15 is the word count; bit 3 (value 8) is
# "elevated privileges are not required to install".
$SummaryInformation = $Database.GetType().InvokeMember(
    "SummaryInformation", "GetProperty", $null, $Database, @(0))
$WordCount = $SummaryInformation.GetType().InvokeMember(
    "Property", "GetProperty", $null, $SummaryInformation, @(15))
if ((($WordCount -band 8) -eq 0)) {
    Write-Error "reason=entitlement_rejected: the package asks for elevation (word count $WordCount)"
    exit 1
}
$RegistryKeyCount = $RegistryRows.Count
$ShortcutCount = (Invoke-MsiQuery "SELECT ``Shortcut`` FROM ``Shortcut``").Count
$UpgradeCode = (Invoke-MsiQuery "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='UpgradeCode'")[0][0]
Write-Host "==> package audit: $RegistryKeyCount HKCU registry rows, $ShortcutCount shortcut, upgrade code $UpgradeCode, no elevation requested"
[System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($Database) | Out-Null
[System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($WindowsInstaller) | Out-Null

if ($SignTool) {
    Write-Host "==> signing the installer"
    Invoke-Sign $Installer
    $SignStatus = "verified"
    $SignDetail = "Authenticode signature verified on the .msi and on both installed executables; uninstall is performed by Windows Installer itself, so there is no generated uninstaller of ours to sign"
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
    installer_tool       = "WiX $WixReported ($Wix)"
    install_scope        = "per-user: %LOCALAPPDATA%\Programs\Plaudon, HKCU only"
    data_directory       = "%APPDATA%\app.plaudon.desktop"
    file_associations    = ".plaudon via the Plaudon.Archive ProgID; plaudon:// URL scheme"
    embedded_version     = $FileVersion
    upgrade_code         = $UpgradeCode
    signature_status     = $SignStatus
    signature_detail     = $SignDetail
    notarization_status  = "not_applicable"
    privilege_audit      = "manifest requests asInvoker; the built database writes HKCU only, leaves ALLUSERS empty and declares that no elevation is required"
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
