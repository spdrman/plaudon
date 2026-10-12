<#
.SYNOPSIS
    A1.1 local speech-to-text benchmark on Windows (spec 2.6, 17.5).

.DESCRIPTION
    Emits one machine-readable result validated against
    schemas/benchmark-result.v1.schema.json. Requires a whisper.cpp binary you
    built or installed yourself; this script never downloads one.

.EXAMPLE
    scripts\bench-whisper.ps1 -Model ggml-base.en-q5_1 -Backend cpu -Audio tests\fixtures\audio\valid\tone-mono-16k.wav
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Model,
    [Parameter(Mandatory = $true)][ValidateSet("cpu", "metal", "cuda", "vulkan")][string]$Backend,
    [Parameter(Mandatory = $true)][string]$Audio,
    [string]$Binary = $env:WHISPER_BIN,
    [string]$Out
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not $Binary) { $Binary = "whisper-cli.exe" }
if (-not (Get-Command $Binary -ErrorAction SilentlyContinue)) {
    throw "whisper binary not found: $Binary (set WHISPER_BIN or pass -Binary)"
}
if (-not (Test-Path $Audio)) { throw "audio file not found: $Audio" }

$modelDir = $env:WHISPER_MODEL_DIR
if (-not $modelDir) { $modelDir = Join-Path $env:LOCALAPPDATA "plaudon\models\whisper" }
$modelPath = Join-Path $modelDir "$Model.bin"
if (-not (Test-Path $modelPath)) { throw "model not found: $modelPath" }

# Refuse to benchmark weights that do not match their manifest.
$digestFile = "$modelPath.sha256"
if (Test-Path $digestFile) {
    $expected = (Get-Content $digestFile -Raw).Split(" ")[0].Trim()
    $actual = (Get-FileHash -Algorithm SHA256 $modelPath).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { throw "model digest mismatch for $Model" }
}

$reader = New-Object System.IO.BinaryReader([System.IO.File]::OpenRead($Audio))
try {
    $bytes = [System.IO.File]::ReadAllBytes($Audio)
} finally {
    $reader.Close()
}
if ($bytes.Length -lt 44 -or [System.Text.Encoding]::ASCII.GetString($bytes, 0, 4) -ne "RIFF") {
    throw "could not determine audio duration; use a WAV input"
}
$sampleRate = [BitConverter]::ToUInt32($bytes, 24)
$byteRate = [BitConverter]::ToUInt32($bytes, 28)
if ($byteRate -eq 0) { throw "malformed WAV header" }
$audioMs = [int](($bytes.Length - 44) * 1000 / $byteRate)
if ($audioMs -le 0) { throw "audio duration is zero" }

$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
$os = (Get-CimInstance Win32_OperatingSystem)
$hostOs = "$($os.Caption) $($os.Version) ($($os.BuildNumber))"
$ram = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$process = Start-Process -FilePath $Binary -ArgumentList @("-m", $modelPath, "-f", $Audio) `
    -NoNewWindow -PassThru -Wait
$stopwatch.Stop()
if ($process.ExitCode -ne 0) { throw "whisper run failed with exit code $($process.ExitCode)" }

if (-not $Out) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $Out = "docs\benchmarks\results\$stamp-$Backend-$Model.json"
}
New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null

[ordered]@{
    schema_version          = 1
    host_os                 = $hostOs
    host_cpu                = $cpu
    host_ram_bytes          = [int64]$ram
    backend                 = $Backend
    model_id                = $Model
    audio_duration_ms       = $audioMs
    wall_clock_ms           = [int]$stopwatch.ElapsedMilliseconds
    peak_rss_bytes          = [int64]$process.PeakWorkingSet64
    cancellation_latency_ms = $null
    segments_reported       = $false
    word_timestamps_reported = $false
    run_on                  = (Get-Date -Format "yyyy-MM-dd")
} | ConvertTo-Json | Set-Content -Encoding UTF8 $Out

Write-Host "wrote $Out"
Write-Host "Set segments_reported and word_timestamps_reported from the engine output you observed."
Write-Host "An unobserved capability stays false; spec 8.3 treats unknown as unsupported."
