<#
.SYNOPSIS
    Builds audcap.exe (the Svelte app in frontend\ is embedded in it).

.DESCRIPTION
    Builds the frontend with npm (frontend\dist), puts the CMake that ships with
    the Visual Studio C++ build tools on PATH (libopus is built with it; not
    needed when LIBOPUS_LIB_DIR points at a prebuilt opus.lib), refuses to build
    while AudCap is running from the output folder (Windows locks the exe),
    then runs cargo build in backend\recorder.

.EXAMPLE
    .\build.ps1                 # release build -> backend\recorder\target\x86_64-pc-windows-msvc\release\audcap.exe
    .\build.ps1 -Test -Start    # run unit tests, build, then start the tray
    .\build.ps1 -Dev            # quick debug build -> backend\recorder\target\x86_64-pc-windows-msvc\debug\audcap.exe
    .\build.ps1 -Jobs 4         # fewer cores (default: half the logical processors)
    .\copy-release.bat          # copy the release exe to C:\Drive\Software\Apps\audio-capture and start it

    If scripts are blocked: powershell -ExecutionPolicy Bypass -File .\build.ps1
#>
[CmdletBinding()]
param(
    # Debug build (fast, no LTO) instead of release.
    [switch]$Dev,
    # Run the frontend tests and `cargo test --lib` first; stop if they fail.
    [switch]$Test,
    # Kill running AudCap processes instead of stopping. An open recording is
    # then finalized as "recovered" on next start; prefer tray menu -> Quit.
    [switch]$Force,
    # Start the tray from the new exe when done.
    [switch]$Start,
    # Parallel jobs for cargo, rustc and the libopus C build. 0 = CARGO_BUILD_JOBS
    # if set, else half the logical processors (all of them made the machine stall).
    [int]$Jobs = 0
)

$ErrorActionPreference = 'Stop'
$crateDir = Join-Path $PSScriptRoot 'backend\recorder'
$frontendDir = Join-Path $PSScriptRoot 'frontend'

# The script changes folders; put the caller back where they were, even on failure.
Push-Location
try {

if ($Jobs -le 0) {
    $Jobs = if ($env:CARGO_BUILD_JOBS -as [int]) { [int]$env:CARGO_BUILD_JOBS }
            else { [Math]::Max(1, [int][Math]::Floor([Environment]::ProcessorCount / 2)) }
}
# cargo hands these out as jobserver tokens to rustc (also its LTO/codegen threads)
# and to the cmake crate (NUM_JOBS); CMAKE_BUILD_PARALLEL_LEVEL caps Ninja directly too.
$env:CARGO_BUILD_JOBS = "$Jobs"
$env:CMAKE_BUILD_PARALLEL_LEVEL = "$Jobs"
Write-Host "Using $Jobs of $([Environment]::ProcessorCount) logical processors"

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

function Invoke-Npm {
    # npm.cmd, not the npm.ps1 shim: the shim re-parses its caller's line and loses splatted args.
    & npm.cmd @args
    if ($LASTEXITCODE -ne 0) { throw "npm $($args -join ' ') failed (exit $LASTEXITCODE)" }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo not found. Install Rust (MSVC toolchain) from https://rustup.rs'
}
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    throw 'Node.js (npm) not found. Install Node.js from https://nodejs.org; it is needed to build the window UI.'
}

if ($env:LIBOPUS_LIB_DIR) {
    Write-Host "Using prebuilt libopus from $env:LIBOPUS_LIB_DIR"
} else {
    $cmake = Find-CMake
    if (-not $cmake) {
        throw 'CMake not found. Without admin rights: `uv tool install cmake` and `uv tool install ninja`. Or install the Visual Studio C++ build tools, or set LIBOPUS_LIB_DIR to a folder whose lib\ has a prebuilt opus.lib.'
    }
    $env:PATH = "$(Split-Path $cmake);$env:PATH"
    Write-Host "Using CMake: $cmake"

    # A portable MSVC (cl.exe on PATH, INCLUDE/LIB set) isn't a registered Visual
    # Studio, so CMake's default Visual Studio generator can't use it; Ninja can.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $hasVs = (Test-Path $vswhere) -and (& $vswhere -latest -products * -property installationPath)
    if (-not $env:CMAKE_GENERATOR -and -not $hasVs) {
        if (-not (Get-Command ninja -ErrorAction SilentlyContinue)) {
            throw 'No Visual Studio install found, so libopus needs the Ninja generator. Install it without admin rights: `uv tool install ninja`.'
        }
        $env:CMAKE_GENERATOR = 'Ninja'
        # cl.exe C1041 (PDB locked) otherwise, when CMake's compiler checks run.
        $env:CFLAGS = "$env:CFLAGS /FS".Trim()
        Write-Host 'Using the Ninja generator with the cl.exe on PATH'
    }
}

# winresource looks for rc.exe in the registry; a portable Windows SDK isn't there.
if (-not $env:RC_PATH) {
    $rc = Get-Command rc.exe -ErrorAction SilentlyContinue
    if ($rc) { $env:RC_PATH = $rc.Source }
}

Write-Host 'Building the frontend...'
Set-Location $frontendDir
$lock = Join-Path $frontendDir 'package-lock.json'
$modules = Join-Path $frontendDir 'node_modules'
if (-not (Test-Path $modules) -or (Get-Item $lock).LastWriteTime -gt (Get-Item $modules).LastWriteTime) {
    Invoke-Npm ci
}
if ($Test) { Invoke-Npm test }
# Rebuild dist only when a frontend input is newer than it. A rebuild rewrites
# every file in dist, which makes cargo recompile and relink audcap even when
# nothing changed. Delete frontend\dist to force it.
$stamp = Join-Path $frontendDir 'dist\index.html'
$stale = -not (Test-Path $stamp)
if (-not $stale) {
    $built = (Get-Item $stamp).LastWriteTime
    $inputs = @(Get-ChildItem $frontendDir -File) + @(Get-ChildItem (Join-Path $frontendDir 'src') -File -Recurse)
    $stale = [bool]($inputs | Where-Object { $_.LastWriteTime -gt $built } | Select-Object -First 1)
}
if ($stale) { Invoke-Npm run build } else { Write-Host 'Frontend unchanged; skipping npm run build.' }
Set-Location $crateDir

$profileDir = if ($Dev) { 'debug' } else { 'release' }
# .cargo\config.toml sets [build] target, so cargo writes under target\<triple>\.
$exe = Join-Path $crateDir "target\x86_64-pc-windows-msvc\$profileDir\audcap.exe"

$running = @(Get-Process audcap -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe })
if ($running.Count -gt 0) {
    if (-not $Force) {
        throw "AudCap is running from $exe, which locks the file. Quit it from the tray menu (or pass -Force)."
    }
    Write-Warning 'Killing running AudCap processes.'
    $running | Stop-Process -Force
    $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

if ($Test) {
    Write-Host 'Running Rust unit tests...'
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
    Write-Host 'Started AudCap.'
}
} finally {
    Pop-Location
}
