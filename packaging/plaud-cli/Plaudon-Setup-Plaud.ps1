<#
.SYNOPSIS
    Guided setup for the official PLAUD Cloud CLI on Windows (spec 7.1, 7.2).

.DESCRIPTION
    Plaudon never signs you in to PLAUD itself and never handles your PLAUD
    credentials. Signing in is done by the vendor's own CLI, in your own shell,
    against the vendor's own OAuth flow. This script does four things and
    nothing else:

      1. checks that Node.js and npm are present
      2. installs @plaud-ai/cli at the one version this build is verified
         against, if that exact version is not already installed
      3. runs `plaud login`, which opens your browser
      4. runs `plaud me` to confirm the session exists

    It prints no token, no session file and no raw credential: everything the
    vendor CLI writes to this terminal is passed through a redactor first.

    Installed beside Plaudon.exe by the .msi, and published as a release asset
    for anyone who unpacked the installer by hand.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File Plaudon-Setup-Plaud.ps1
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The single version of the vendor CLI this build will drive. It is the version
# recorded in crates/source-plaud-cli/cli-compatibility.yaml, which is compiled
# into the application; anything else is refused by the application at runtime
# (INCOMPATIBLE_VERSION), so installing anything else would only move the
# failure later.
$PlaudCliPackage = '@plaud-ai/cli'
$PlaudCliVersion = '0.3.15'

function Write-Step([string]$Message) {
    Write-Host ''
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Problem([string]$Message) {
    Write-Host ''
    Write-Host "!!  $Message" -ForegroundColor Red
}

# Everything the vendor CLI prints goes through here. A bearer token, a JWT or
# a `token: <long opaque string>` field never reaches the terminal or a
# scrollback buffer, whatever a future version of that CLI decides to print.
function Protect-Secret {
    param([Parameter(ValueFromPipeline = $true)][string]$Line)
    process {
        if ($null -eq $Line) { return }
        $redacted = [regex]::Replace(
            $Line,
            'eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_.-]{8,}',
            '[redacted token]')
        $redacted = [regex]::Replace(
            $redacted,
            '(?i)((token|secret|bearer|api[_-]?key|password|refresh)[^A-Za-z0-9]{0,4})[A-Za-z0-9._~+/-]{12,}=*',
            '$1[redacted]')
        Write-Host $redacted
    }
}

function Test-Command([string]$Name) {
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Stop-WithNodeGuidance {
    Write-Problem 'Node.js is required and was not found.'
    Write-Host @'
The official PLAUD Cloud CLI is an npm package, so it needs Node.js.

  winget install OpenJS.NodeJS.LTS
  Or:  https://nodejs.org/en/download

Open a new terminal afterwards so the PATH change takes effect, then run this
script again.
'@
    exit 1
}

# `plaud version` prints `plaud <version>` on its first line; `--version` is
# rejected as an unknown option by 0.3.15, which is why it is not used here.
function Get-InstalledPlaudVersion {
    if (-not (Test-Command 'plaud')) { return $null }
    $output = & plaud version 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $output) { return $null }
    $first = @($output)[0]
    $match = [regex]::Match([string]$first, '^plaud\s+([0-9][0-9A-Za-z.+-]*)')
    if ($match.Success) { return $match.Groups[1].Value }
    return $null
}

Write-Step 'Checking for Node.js'
if (-not (Test-Command 'node')) { Stop-WithNodeGuidance }
if (-not (Test-Command 'npm')) { Stop-WithNodeGuidance }
$nodeVersion = (& node --version 2>$null) -join ''
$npmVersion = (& npm --version 2>$null) -join ''
Write-Host "node $nodeVersion, npm $npmVersion"

Write-Step "Checking for $PlaudCliPackage $PlaudCliVersion"
$current = Get-InstalledPlaudVersion
if ($current -eq $PlaudCliVersion) {
    Write-Host "already installed: plaud $current"
}
else {
    if ($current) {
        Write-Host "found plaud $current; Plaudon drives $PlaudCliVersion only and refuses any other"
        Write-Host "version, so this installs $PlaudCliVersion over it."
    }
    else {
        Write-Host 'not installed; installing it now.'
    }
    Write-Host "running: npm install -g $PlaudCliPackage@$PlaudCliVersion"
    & npm install -g "$PlaudCliPackage@$PlaudCliVersion" 2>&1 | Protect-Secret
    if ($LASTEXITCODE -ne 0) {
        Write-Problem "npm could not install $PlaudCliPackage@$PlaudCliVersion."
        Write-Host @'
This is usually one of three things:

  * no network, or a proxy npm does not know about
  * a global prefix you cannot write to — `npm config get prefix` and either
    fix its permissions or set one under your profile:
        npm config set prefix "$env:LOCALAPPDATA\npm-global"
        $env:PATH = "$env:LOCALAPPDATA\npm-global;$env:PATH"
  * the version was unpublished upstream, in which case Plaudon cannot drive
    the vendor CLI at all until its compatibility table is updated and the
    application rebuilt.

Nothing has been signed in to. Fix the above and run this script again.
'@
        exit 1
    }
    $current = Get-InstalledPlaudVersion
    if ($current -ne $PlaudCliVersion) {
        $reported = if ($current) { $current } else { 'nothing' }
        Write-Problem "after installing, ``plaud version`` reports '$reported', not $PlaudCliVersion."
        Write-Host @'
Another `plaud` is probably earlier on your PATH — `Get-Command plaud -All` and
`npm root -g` will show which. Plaudon fails closed on an unrecognised version,
so resolve this before signing in.
'@
        exit 1
    }
    Write-Host "installed: plaud $current"
}

Write-Step 'Signing in to PLAUD'
Write-Host @'
`plaud login` opens your browser and signs you in to your own PLAUD account.
This is the vendor's flow: Plaudon never sees your password, and the session it
creates belongs to the CLI, not to Plaudon. Plaudon reads what that session can
already read, and the application cannot sign you out again — the vendor CLI's
`logout` is not something this build exercises.
'@
Write-Host ''
& plaud login 2>&1 | Protect-Secret
if ($LASTEXITCODE -ne 0) {
    Write-Problem '`plaud login` did not complete.'
    Write-Host @'
Nothing was changed. Common causes: the browser window was closed before the
flow finished, no default browser is set for this user, or the account requires
a step the CLI cannot do here. Run `plaud login` yourself to see the vendor's
own message, then run this script again.
'@
    exit 1
}

Write-Step 'Confirming the session'
& plaud me 2>&1 | Protect-Secret
if ($LASTEXITCODE -ne 0) {
    Write-Problem '`plaud me` did not report a signed-in user.'
    Write-Host @'
The login flow reported success but the session cannot be read back. Try
`plaud login` again; if it keeps happening, the vendor CLI's session store is
the place to look, not Plaudon.
'@
    exit 1
}

Write-Step 'Done'
Write-Host @'
The PLAUD source in Plaudon can now enumerate and download your own recordings.

Next steps:

  1. Open Plaudon.
  2. Local-only Mode is on from the first launch and blocks every outbound
     call, including this one. Turn it off before using the PLAUD source if you
     want the application to reach the vendor at all.
  3. Settings -> Sources -> PLAUD Cloud CLI: the version check runs there, and
     it will say 0.3.15 if this finished cleanly.

This terminal printed no token. If you want the session gone, use the vendor
CLI's own `logout`; Plaudon cannot revoke it for you.

Windows support in Plaudon is untested: the installer and the application are
built on a Windows runner but no automated suite runs them on Windows.
'@
