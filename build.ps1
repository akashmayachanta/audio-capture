<#
.SYNOPSIS
    Builds meetrec.exe.

.DESCRIPTION
    Puts the CMake that ships with the Visual Studio C++ build tools on PATH
    (libopus is built with it), refuses to build while MeetRec is running from
    the output folder (Windows locks the exe), then runs cargo build.

.EXAMPLE
    .\build.ps1                 # release build -> target\release\meetrec.exe
    .\build.ps1 -Test -Start    # run unit tests, build, then start the tray
    .\build.ps1 -Dev            # quick debug build -> target\debug\meetrec.exe

    If scripts are blocked: powershell -ExecutionPolicy Bypass -File .\build.ps1
#>
[CmdletBinding()]
param(
    # Debug build (fast, no LTO) instead of release.
    [switch]$Dev,
    # Run `cargo test --lib` first; stop if it fails.
    [switch]$Test,
    # Kill running MeetRec processes instead of stopping. An open recording is
    # then finalized as "recovered" on next start; prefer tray menu -> Quit.
    [switch]$Force,
    # Start the tray from the new exe when done.
    [switch]$Start
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

function Find-CMake {
    $cmd = Get-Command cmake -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -find 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe' | Select-Object -First 1
        if ($found) { return $found }
    }
    $found = Get-ChildItem "$env:ProgramFiles\CMake\bin\cmake.exe", "${env:ProgramFiles(x86)}\Microsoft Visual Studio\*\*\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe" `
        -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { return $found.FullName }
    return $null
}

function Invoke-Cargo {
    & cargo @args
    if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed (exit $LASTEXITCODE)" }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo not found. Install Rust (MSVC toolchain) from https://rustup.rs'
}

$cmake = Find-CMake
if (-not $cmake) {
    throw 'CMake not found. Install the Visual Studio C++ build tools (they include CMake) or CMake itself.'
}
$env:PATH = "$(Split-Path $cmake);$env:PATH"
Write-Host "Using CMake: $cmake"

$profileDir = if ($Dev) { 'debug' } else { 'release' }
$exe = Join-Path $PSScriptRoot "target\$profileDir\meetrec.exe"

$running = @(Get-Process meetrec -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe })
if ($running.Count -gt 0) {
    if (-not $Force) {
        throw "MeetRec is running from $exe, which locks the file. Quit it from the tray menu (or pass -Force)."
    }
    Write-Warning 'Killing running MeetRec processes.'
    $running | Stop-Process -Force
    $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

if ($Test) {
    Write-Host 'Running unit tests...'
    Invoke-Cargo test --lib
}

Write-Host "Building ($profileDir)..."
$sw = [Diagnostics.Stopwatch]::StartNew()
if ($Dev) { Invoke-Cargo build } else { Invoke-Cargo build --release }
$sw.Stop()

$size = (Get-Item $exe).Length / 1MB
Write-Host ("Built {0} ({1:N1} MB) in {2:mm\:ss}" -f $exe, $size, $sw.Elapsed) -ForegroundColor Green

if ($Start) {
    Start-Process $exe
    Write-Host 'Started MeetRec.'
}
