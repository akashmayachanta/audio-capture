@echo off
rem Installs the latest release build of AudCap (build it with build.ps1 first)
rem into C:\Drive\Software\Apps\audio-capture:
rem   1. stops the transcriber and every running AudCap (tray and window)
rem   2. deletes the old exe there and the obsolete build folders from before the
rem      x86_64-pc-windows-msvc target (repo target\, backend\recorder\target\release|debug)
rem   3. copies the new exe, links transcriber\ to the repo's, starts the tray.
rem With "Start with Windows" on, the new tray points the sign-in entry at itself.
setlocal
set "REPO=%~dp0"
set "SRC=%REPO%backend\recorder\target\x86_64-pc-windows-msvc\release\audcap.exe"
set "DEST=C:\Drive\Software\Apps\audio-capture"

if not exist "%SRC%" (
    echo No release build at "%SRC%".
    echo Run build.ps1 first.
    pause
    exit /b 1
)

echo Stopping the transcriber...
rem Graceful first (POST /api/shutdown); a server from an older build lacks it, so
rem then end the uv/python tree running "python -m app" from the repo's .venv.
powershell -NoProfile -Command ^
  "try { Invoke-RestMethod -Method Post -Uri http://127.0.0.1:8765/api/shutdown -ContentType 'application/json' -Body '{}' -TimeoutSec 3 | Out-Null; Start-Sleep 2 } catch {};" ^
  "Get-CimInstance Win32_Process -Filter \"Name='python.exe'\" | Where-Object { $_.CommandLine -like '*-m app*' -and $_.ExecutablePath -like '*\transcriber\.venv\*' } | ForEach-Object {" ^
  "  $parent = Get-CimInstance Win32_Process -Filter \"ProcessId=$($_.ParentProcessId)\";" ^
  "  $root = if ($parent -and $parent.Name -eq 'uv.exe') { $parent.ProcessId } else { $_.ProcessId };" ^
  "  taskkill /PID $root /T /F | Out-Null };"

echo Stopping AudCap (an open recording is finalized as "recovered" on the next start)...
powershell -NoProfile -Command ^
  "$p = Get-Process audcap -ErrorAction SilentlyContinue; $p | Stop-Process -Force; $p | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue"

echo Deleting old files...
if exist "%DEST%\audcap.exe" del /F /Q "%DEST%\audcap.exe"
for %%D in ("%REPO%target" "%REPO%backend\recorder\target\release" "%REPO%backend\recorder\target\debug") do (
    if exist "%%~D" (
        rmdir /S /Q "%%~D"
        echo   removed %%~D
    )
)

if not exist "%DEST%" mkdir "%DEST%"
copy /Y "%SRC%" "%DEST%\audcap.exe" >nul
if errorlevel 1 (
    echo Copying to "%DEST%" failed.
    pause
    exit /b 1
)
echo Copied to %DEST%\audcap.exe

rem The window's Start button looks for transcriber\ next to the exe: link it to
rem the repo's, so the copy shares its Python environment and settings.toml.
if not exist "%DEST%\transcriber" mklink /J "%DEST%\transcriber" "%REPO%backend\transcriber" >nul && echo Linked %DEST%\transcriber to the repo's transcriber.

start "" "%DEST%\audcap.exe"
echo Started AudCap from %DEST%. Start the transcriber from its window (Settings - Transcription).
