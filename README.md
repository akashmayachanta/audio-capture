# AudCap

A Windows tray app that records meetings: **your microphone on the left
channel, system audio (everyone else) on the right**, so transcripts can label
"Me" and "Others" without speaker detection. It also does dictation and
transcribes recordings, on this machine or through a speech API.

- **Auto-record** when an allow-listed app (Teams, Zoom, Webex, Slack, Chrome,
  Edge, Firefox, or any exe you add) starts using the microphone.
- **Manual** start/stop, pause, split and markers from the tray or hotkeys.
- **Dictation**: hold Right Alt, speak, release; the text is typed where your
  cursor is.
- **Transcription**: the local transcriber (Python, on-device) or any
  OpenAI-compatible speech API.

## Using it

Run `audcap.exe`. The tray icon is grey (idle), blue (armed, waiting for a
meeting), red (recording) or amber (paused).

| Hotkey     | Action         |
|------------|----------------|
| Ctrl+Alt+R | start / stop   |
| Ctrl+Alt+P | pause / resume |
| Ctrl+Alt+S | split now      |
| Ctrl+Alt+M | add marker     |

**Recordings…** and **Settings…** in the tray menu open the AudCap window.

- **Recordings**: recordings by day. Select one to play it (Listen: Both / Me /
  Others) and read its transcript (click a timestamp to play from there).
  **Transcribe**, **Redo**, **Cancel** and **Delete** are beside each recording.
- **Settings**, in three tabs: **Recording** (capture devices, folder, quality,
  auto-record apps, hotkeys, start with Windows), **Dictation** (keys, engine,
  speech margin, microphone test) and **Transcription** (speech API, the local
  transcriber's models). **Save** writes `config.toml`; the tray picks it up right away.

## Dictation

| Gesture | What happens |
|---|---|
| Hold **Right Alt**, speak, release | the text is pasted where the cursor is |
| Double-tap **Right Alt** | hands-free until you double-tap again |
| **Esc** | cancels |

A round microphone badge shows the state (listening, transcribing, error).
Dictation uses the local transcriber; if it isn't running, the first dictation
starts it. The tray's **Speech service** menu starts, stops or switches its
model. Keys and paste mode are in **Settings → Dictation**. Phrases end at
pauses: speech has to be **Speech margin** dB (default 10) above the room's
noise, which AudCap measures as you talk. **Test microphone** on the same tab
shows the level, the noise floor and the threshold live. Clips are kept in
`<output_dir>\dictation\` unless you turn off **Keep the audio**.

## Transcription

Nothing is transcribed by itself: click **Transcribe** on a recording. The mic
and system channels are transcribed separately, so every line is **Me** or
**Others**. When both options below are set up, **Transcribe with** on the
Recordings page picks one.

**Local transcriber** (`backend\transcriber\`, Python, [uv](https://docs.astral.sh/uv/)).
Start it with **Start** in Settings → Transcription (it keeps running after the
window closes), or by hand:

```powershell
cd backend\transcriber
uv sync
uv run python -m app         # http://127.0.0.1:8765/
```

Load a model in Settings → Transcription: **Cactus Whistle** (default, fast,
several languages) or **Underdog Bark 3.0** (English, more accurate; uses Intel
graphics when available). Its own settings are in
`backend\transcriber\settings.toml`; models are cached in
`%LOCALAPPDATA%\AudCap\transcriber`. It must read the same folder AudCap saves
to.

**Speech API**: any OpenAI-compatible `/audio/transcriptions` endpoint (OpenAI,
Groq, a local Whisper server). Enter the base URL, API key and model in
Settings → Speech API and press **Test connection**. Dictation can use it too
(Settings → Dictation → **Transcribe with: Speech API**): the whole dictation
goes in one request when you stop talking, so the transcriber isn't needed. The tray sends each
speaker's audio in pieces of up to 10 minutes, skipping silence. Models that
don't return timestamps (e.g. `gpt-4o-transcribe`) get 30-second pieces
instead. Jobs run in the tray, one at a time, and keep going after the window
closes.

Either way the transcript is `<name>.md` next to the recording:

```markdown
# Transcript: 2026-10-19_1430_Teams
- **Recording:** `2026-10-19_1430_Teams.opus`
…
**[00:01:23] Me:** Hello everyone.

**[00:01:25] Others:** Hi, can you hear me?
```

## Output

```
<output_dir>/2026/10/2026-10-19_1430_Teams.opus   Ogg Opus, 48 kHz (L = mic, R = system)
<output_dir>/2026/10/2026-10-19_1430_Teams.json   sidecar: times, apps, devices, channels, pauses, markers, status
<output_dir>/2026/10/2026-10-19_1430_Teams.md     transcript, once made
```

- Files are `*.part` while recording. After a crash they are finished on the
  next start with `"status": "recovered"`.
- Auto recordings shorter than `min_duration_secs` (30 s) are discarded.
- Stereo files use two separate Opus streams. Windows Media Player plays only
  the mic; use the AudCap window, VLC or ffmpeg.

## Config

`%APPDATA%\AudCap\config.toml` (or the path in `AUDCAP_CONFIG`). Every field is
optional; Settings edits all of them.

```toml
capture_mode = "both"          # both | mic | system
mic_device = ""                # empty = Windows default
system_device = ""             # speakers/headset to record "others" from; empty = Windows default
output_dir = "C:\\Users\\me\\Documents\\AudCap"
bitrate_kbps = 24              # per channel
auto_record = true
trigger_apps = ["Teams", "Zoom", "Webex", "Slack", "Chrome", "Edge", "Firefox"]
release_grace_secs = 10        # stop this long after the meeting app releases the mic
min_duration_secs = 30
start_with_windows = false
transcriber_dir = ""           # empty = found next to the exe or in the repo
web_port = 8766                # local API for other apps; 0 = off; read when the tray starts

[hotkeys]
start_stop = "ctrl+alt+R"
pause = "ctrl+alt+P"
split = "ctrl+alt+S"
marker = "ctrl+alt+M"

[dictation]
enabled = true
hold_key = "RightAlt"          # "" = off
toggle_key = "RightAlt"        # double-tap; "" = off
keep_audio = true
max_secs = 300
paste_mode = "live"            # live = as you speak (hands-free); end = all at once
engine = "local"               # local = the transcriber; api = [speech_api], whole clip on release
noise_margin_db = 10           # speech must be this many dB above the room noise (3-30)

[speech_api]                   # empty = not set up
base_url = ""                  # e.g. "https://api.openai.com/v1"
api_key = ""
model = ""                     # e.g. "whisper-1"
```

Logs: `%APPDATA%\AudCap\logs` (`AUDCAP_LOG=debug` for more).

## Local API

The tray serves the recordings to other apps on this machine (such as
daily-driver's Recordings page) at `http://127.0.0.1:<web_port>/api/`, loopback
only:

| Route | |
|---|---|
| `GET recordings` | the list |
| `GET/PUT/DELETE transcript?id=` | a recording's `.md` |
| `DELETE recording?id=` | the recording, sidecar and transcript |
| `GET audio?id=` | the `.opus` (Range for seeking) |
| `GET pcm?id=&ch=&start=&secs=` | one channel as 16 kHz WAV |
| `GET transcriber`, `POST transcriber/start` | start the local transcriber |
| `GET/PUT speech/settings`, `POST speech/test` | the speech API settings |
| `GET speech/jobs`, `POST speech/transcribe`, `POST speech/cancel` | speech API jobs |

## Layout

```
frontend/               Svelte app (the window's UI) -> frontend/dist, embedded in the exe
backend/recorder/       Rust crate -> audcap.exe (tray, capture, window, local API, speech API jobs)
backend/transcriber/    local transcriber (Python, uv)
build.ps1               builds the frontend, then the exe
copy-release.bat        installs the release exe in C:\Drive\Software\Apps\audio-capture and starts it
```

## Building

Needs Rust (MSVC toolchain), the MSVC C++ build tools, CMake (for libopus) and
Node.js. Without admin rights: `uv tool install cmake` and `uv tool install
ninja` with a portable MSVC on `PATH`; `build.ps1` handles the rest.

```powershell
.\build.ps1 -Test -Start     # tests, release build, start the tray
.\copy-release.bat           # install that exe and start it
cd backend\recorder; cargo test --lib   # Rust unit tests
cd frontend; npm test                   # UI unit tests
```

Quit AudCap from the tray before rebuilding: Windows locks the running exe.
`build.ps1` options: `-Dev` (debug build), `-Test`, `-Start`, `-Force` (kill a
running AudCap).
