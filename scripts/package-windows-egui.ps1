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
# The installer icon (ARP entry, Start menu shortcut) is the brand kit's, the
# same file the executable resource links; it is staged into the WiX bind path
# under the name plaudon-egui.wxs references.
$Icon = Join-Path $RepoRoot "brand/kit/icons/desktop/plaudon.ico"
if (-not (Test-Path $Icon)) { Write-Error "reason=missing_binary: $Icon is absent; the brand kit must be checked out"; exit 1 }

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

# CARGO_TARGET_DIR is honoured because on Windows it is sometimes the only
# way to build at all: ggml's Vulkan backend nests its CMake scratch
# directories deep enough that MSBuild's FileTracker exceeds MAX_PATH when
# the target directory sits inside a checkout path of any length.
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $RepoRoot "target" }
$Exe = Join-Path $TargetDir "release/plaudon-egui.exe"
$Cli = Join-Path $TargetDir "release/plaudon.exe"
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
#
# It runs under Windows PowerShell 5.1 rather than in this process: PowerShell
# 7's COM binder does not return data for the `Record.StringData`
# parameterized property — it reports a type mismatch, or hands back empty
# strings, which is the one failure mode an audit must not have. 5.1 ships
# with Windows, so this needs nothing installed.
$AuditScript = Join-Path $OutFull "msi-audit.ps1"
@'
param([Parameter(Mandatory = $true)][string]$Msi)
$ErrorActionPreference = "Stop"
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.GetType().InvokeMember(
    "OpenDatabase", "InvokeMethod", $null, $installer, @($Msi, 0))
function Get-Column([string]$sql) {
    $view = $database.GetType().InvokeMember("OpenView", "InvokeMethod", $null, $database, @($sql))
    $view.GetType().InvokeMember("Execute", "InvokeMethod", $null, $view, $null) | Out-Null
    $values = @()
    while ($true) {
        $record = $view.GetType().InvokeMember("Fetch", "InvokeMethod", $null, $view, $null)
        # `-not $record` would be asking PowerShell to coerce a COM Record to
        # a boolean through its default member, which is the record's own
        # first field: a row whose first field is empty would end the read
        # early and the audit would see nothing.
        if ($null -eq $record) { break }
        $values += [string]$record.GetType().InvokeMember(
            "StringData", "GetProperty", $null, $record, @(1))
    }
    $view.GetType().InvokeMember("Close", "InvokeMethod", $null, $view, $null) | Out-Null
    # No comma: `return , $values` hands back an array *containing* the array,
    # which the JSON below then nests and the caller counts as one row.
    return $values
}
$summary = $database.GetType().InvokeMember("SummaryInformation", "GetProperty", $null, $database, @(0))
[pscustomobject]@{
    roots        = @(Get-Column "SELECT ``Root`` FROM ``Registry``")
    keys         = @(Get-Column "SELECT ``Key`` FROM ``Registry``")
    allusers     = @(Get-Column "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='ALLUSERS'")
    shortcuts    = @(Get-Column "SELECT ``Shortcut`` FROM ``Shortcut``")
    upgradecodes = @(Get-Column "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='UpgradeCode'")
    wordcount    = [int]$summary.GetType().InvokeMember("Property", "GetProperty", $null, $summary, @(15))
} | ConvertTo-Json -Compress
'@ | Set-Content -Path $AuditScript -Encoding utf8

$AuditJson = & "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
    -NoProfile -ExecutionPolicy Bypass -File $AuditScript -Msi $Installer
if ($LASTEXITCODE -ne 0 -or -not $AuditJson) {
    Write-Error "reason=missing_tool: the built package could not be read back for auditing"
    exit 1
}
$Audit = ($AuditJson -join "") | ConvertFrom-Json

# Registry root 2 is HKLM and -1 is "whichever hive the install scope picks",
# which for a package that must never be machine-wide is just as wrong.
$RegistryRoots = @($Audit.roots)
$RegistryKeys = @($Audit.keys)
# An audit that reads nothing passes everything, so the row count is checked
# against the source rather than against zero: every RegistryValue in the
# .wxs must have become a row in the database.
$ExpectedRegistryRows = ([regex]::Matches($WxsText, "<RegistryValue\b")).Count
if ($RegistryRoots.Count -lt $ExpectedRegistryRows) {
    Write-Error "reason=missing_binary: the built package has $($RegistryRoots.Count) Registry rows, and the .wxs writes $ExpectedRegistryRows; the association claim did not survive the build, or could not be read back"
    exit 1
}
for ($Index = 0; $Index -lt $RegistryRoots.Count; $Index++) {
    if ($RegistryRoots[$Index] -ne "1") {
        Write-Error "reason=entitlement_rejected: the built package writes registry root $($RegistryRoots[$Index]) at '$($RegistryKeys[$Index])'; a per-user install writes HKCU (1) and nothing else"
        exit 1
    }
}
$AllUsers = @($Audit.allusers)
if ($AllUsers.Count -ne 0 -and $AllUsers[0] -ne "") {
    Write-Error "reason=entitlement_rejected: ALLUSERS is '$($AllUsers[0])'; a per-user install leaves it empty"
    exit 1
}
# Summary information property 15 is the word count; bit 3 (value 8) is
# "elevated privileges are not required to install".
$WordCount = [int]$Audit.wordcount
if (($WordCount -band 8) -eq 0) {
    Write-Error "reason=entitlement_rejected: the package asks for elevation (word count $WordCount)"
    exit 1
}
$Shortcuts = @($Audit.shortcuts)
if ($Shortcuts.Count -ne 1) {
    Write-Error "reason=entitlement_rejected: the built package carries $($Shortcuts.Count) shortcuts; it installs exactly one, in the Start Menu"
    exit 1
}
$UpgradeCodes = @($Audit.upgradecodes)
if ($UpgradeCodes.Count -ne 1) {
    Write-Error "reason=missing_binary: the built package has no UpgradeCode, so it could never upgrade itself"
    exit 1
}
$UpgradeCode = $UpgradeCodes[0]
Write-Host "==> package audit: $($RegistryRoots.Count) HKCU registry rows, $($Shortcuts.Count) shortcut, upgrade code $UpgradeCode, no elevation requested"
Remove-Item $AuditScript -Force

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
    # Not Write-Error: $ErrorActionPreference is Stop, so a written error
    # terminates the script with status 1 and the deliberate 3 — "built, NOT
    # publishable" — would never reach the caller that keys off it.
    [Console]::Error.WriteLine("reason=missing_credential: the installer is built but NOT publishable")
    exit 3
}
