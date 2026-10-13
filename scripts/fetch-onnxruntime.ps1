# Vendor the pinned ONNX Runtime build the `onnx` feature links, on Windows
# (ADR-0081 3, issue #283). The bash twin is scripts/fetch-onnxruntime.sh and
# the two must stay in step: the URL, the digest and the member name below are
# the same values compiled into crates/ai-speaker/src/runtime.rs and tabulated
# in docs/licensing/third-party-register.md, and
# `plaudon-integration-tests --test licensing_register` fails when they drift.
#
# Why this script exists at all: `ort`'s `download-binaries` feature fetches a
# prebuilt ONNX Runtime over TLS from the build script. This project does not
# enable it - a release build must not reach a CDN while it compiles - so the
# archive is fetched once, here, by exact URL and digest, and the build links
# it through ORT_LIB_LOCATION. A digest mismatch deletes the file and fails.
#
#   pwsh scripts/fetch-onnxruntime.ps1 [-Target <triple>] [-Dir <path>]
#
# Requires `python` on PATH: the dist is a raw LZMA2 stream (no .xz container)
# wrapping a tar, which neither PowerShell nor `tar` can decode, and Python's
# `lzma` module can with the same 64 MiB dictionary ort-sys uses.
#
# This script has never been executed on Windows by this project: no Windows
# x86-64 machine exists in its tooling (docs/PRECOMMIT.md 3,
# docs/release/voice-profiles-evidence.md). It is the exact next step for
# whoever has one, and its first run is what will confirm or correct it.

[CmdletBinding()]
param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Dir = "vendor/onnxruntime"
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

# ONNX Runtime 1.31.0, MIT, (c) Microsoft Corporation.
$Dists = @{
    "x86_64-pc-windows-msvc" = @{
        Url     = "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/x86_64-pc-windows-msvc+directml.tar.lzma2"
        Sha256  = "f99aaef23f12986abb05924c944d8433521263ad756b462df762a5a982c98405"
        Library = "onnxruntime.lib"
    }
    "aarch64-apple-darwin"   = @{
        Url     = "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/aarch64-apple-darwin+coreml.tar.lzma2"
        Sha256  = "4033efca0e6c1606bd2ec3870d4cb19d0dba0ad0160dbfb616e66cae969ed68c"
        Library = "libonnxruntime.a"
    }
}

if (-not $Dists.ContainsKey($Target)) {
    Write-Error "no ONNX Runtime archive is vendored for '$Target'. The two reference platforms are aarch64-apple-darwin and x86_64-pc-windows-msvc (ADR-0081 4); adding a third means adding its digest to crates/ai-speaker/src/runtime.rs and the licence register first."
}
$Dist = $Dists[$Target]

$Python = Get-Command python -ErrorAction SilentlyContinue
if (-not $Python) { $Python = Get-Command python3 -ErrorAction SilentlyContinue }
if (-not $Python) {
    Write-Error "python is required to decompress the raw LZMA2 dist"
}

$Into = Join-Path $Dir $Target
New-Item -ItemType Directory -Force -Path $Into | Out-Null
$Archive = Join-Path $Into ([System.IO.Path]::GetFileName($Dist.Url))

if (Test-Path $Archive) {
    Write-Host "already present: $Archive"
} else {
    Write-Host "downloading $($Dist.Url)"
    Invoke-WebRequest -Uri $Dist.Url -OutFile "$Archive.part" -UseBasicParsing
    Move-Item "$Archive.part" $Archive
}

$Observed = (Get-FileHash -Algorithm SHA256 -Path $Archive).Hash.ToLower()
if ($Observed -ne $Dist.Sha256) {
    Remove-Item -Force $Archive
    Write-Error "checksum mismatch for $Archive`n  expected $($Dist.Sha256)`n  observed $Observed"
}
Write-Host "verified $Archive ($Observed)"

$Library = Join-Path $Into $Dist.Library
if (-not (Test-Path $Library)) {
    $Tar = "$Archive.tar"
    $Script = @'
import lzma, sys

decompressor = lzma.LZMADecompressor(
    format=lzma.FORMAT_RAW,
    filters=[{"id": lzma.FILTER_LZMA2, "dict_size": 1 << 26}],
)
with open(sys.argv[1], "rb") as source, open(sys.argv[2], "wb") as target:
    while True:
        chunk = source.read(1 << 20)
        if not chunk:
            break
        target.write(decompressor.decompress(chunk))
'@
    $ScriptPath = Join-Path $Into "unlzma2.py"
    Set-Content -Path $ScriptPath -Value $Script -Encoding UTF8
    & $Python.Source $ScriptPath $Archive $Tar
    if ($LASTEXITCODE -ne 0) { Write-Error "decompressing $Archive failed" }
    tar -x -f $Tar -C $Into
    if ($LASTEXITCODE -ne 0) { Write-Error "extracting $Tar failed" }
    Remove-Item -Force $Tar, $ScriptPath
}
if (-not (Test-Path $Library)) {
    Write-Error "the archive did not contain $($Dist.Library)"
}
Write-Host "extracted $Library"

$Absolute = (Resolve-Path $Into).Path
Write-Host ""
Write-Host "ONNX Runtime 1.31.0 for $Target is vendored. To build with it:"
Write-Host ""
Write-Host "  `$env:ORT_LIB_LOCATION = `"$Absolute`""
Write-Host "  cargo build --offline -p plaudon-ai-speaker --features onnx"
Write-Host ""
Write-Host "Attribution this binary obliges (already in NOTICE):"
Write-Host "  * ONNX Runtime, (c) Microsoft Corporation, MIT licence."
Write-Host "  * The Windows build additionally carries DirectML, (c) Microsoft"
Write-Host "    Corporation, which ships with Windows and is linked, not redistributed."
