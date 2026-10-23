<#
.SYNOPSIS
    Creates the Python environment used by MeetRec transcription.

.DESCRIPTION
    Builds a venv at <cache_root>\venv and installs OpenVINO, numpy and
    huggingface_hub into it. The cache root is %LOCALAPPDATA%\MeetRec unless
    MEETREC_CACHE or -CacheRoot says otherwise. The model itself is downloaded
    on first use (or now, with -DownloadModel).

.EXAMPLE
    .\tools\transcribe\setup.ps1 -DownloadModel
#>
[CmdletBinding()]
param(
    [string]$CacheRoot = $(if ($env:MEETREC_CACHE) { $env:MEETREC_CACHE } else { Join-Path $env:LOCALAPPDATA 'MeetRec' }),
    # Python used to create the venv (needs a Python that OpenVINO ships wheels for).
    [string]$Python = 'python',
    # Also fetch the model weights (~1.2 GB) now instead of on first transcription.
    [switch]$DownloadModel
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$venv = Join-Path $CacheRoot 'venv'
$py = Join-Path $venv 'Scripts\python.exe'

if (-not (Test-Path $py)) {
    Write-Host "Creating venv at $venv"
    New-Item -ItemType Directory -Force $CacheRoot | Out-Null
    & $Python -m venv $venv
    if ($LASTEXITCODE -ne 0) { throw 'venv creation failed' }
}

Write-Host 'Installing requirements'
& $py -m pip install --disable-pip-version-check -q -r (Join-Path $here 'requirements.txt')
if ($LASTEXITCODE -ne 0) { throw 'pip install failed' }

& $py -I -c "import openvino as ov; c = ov.Core(); print('OpenVINO', ov.__version__); [print(' ', d, '-', c.get_property(d, 'FULL_DEVICE_NAME')) for d in c.available_devices]"

if ($DownloadModel) {
    & $py (Join-Path $here 'transcribe.py') --cache-dir $CacheRoot --download-only
    if ($LASTEXITCODE -ne 0) { throw 'model download failed' }
}
Write-Host "Done. Cache root: $CacheRoot"
