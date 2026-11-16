@echo off
rem Starts the transcription API (http://127.0.0.1:8765/ by default) used by the MeetRec window.
rem Close this window to stop it.
setlocal
cd /d "%~dp0"

where uv >nul 2>nul
if errorlevel 1 (
    echo uv was not found on PATH. Install it from https://docs.astral.sh/uv/ and try again.
    pause
    exit /b 1
)

rem uv run installs or updates the environment from uv.lock first when needed.
uv run python -m app
if errorlevel 1 pause
