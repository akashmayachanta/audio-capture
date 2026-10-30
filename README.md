# MeetRec

A small Windows tray app that records meetings to files: **your microphone on the
left channel, system audio (everyone else) on the right**. A separate
transcription/notes app can then label "me" vs "others" without speaker
diarization.

- **Auto-record**: starts when an allow-listed app (Teams, Zoom, Webex, Slack,
  Chrome, Edge, Firefox, or any exe you add) starts using the microphone. Stops
  after the mic has been released for `release_grace_secs` (default 10 s).
- **Manual**: start/stop, pause/resume, split and markers from the tray menu or
  hotkeys. Manual recordings are only stopped manually.
- **Capture mode**: mic + system (stereo), mic only or system only (mono).

## Using it

Run `meetrec.exe`. The tray icon shows the state:

| Icon  | State                                   |
|-------|-----------------------------------------|
| grey  | idle (auto-record off)                  |
| blue  | armed, waiting for a meeting            |
| red   | recording                               |
| amber | paused                                  |

| Hotkey     | Action          |
|------------|-----------------|
| Ctrl+Alt+R | start / stop    |
| Ctrl+Alt+P | pause / resume  |
| Ctrl+Alt+S | split now       |
| Ctrl+Alt+M | add marker      |

A toast appears when an auto recording starts. **Settings…** in the tray menu
opens the settings window (`meetrec.exe --settings`). It edits the config file,
and the tray picks up changes live.

**Recordings…** opens the same window on its Recordings tab
(`meetrec.exe --recordings`). It lists your recordings, newest first. Click one
to load it, or double-click to play it. The controls are Play/Pause, Stop, a
seek bar, and volume (up to 200% for quiet mics). You can listen to **Both**
sides mixed into both ears, **Me only** (the mic, left channel), or **Others
only** (system audio, right channel).

Transcripts live in a separate local web page; see [Transcription](#transcription).

## Transcription

Manual, on-device speech-to-text in a small Python web app (`transcriber\`,
FastAPI, set up with [uv](https://docs.astral.sh/uv/)). It is separate from the
recorder: `meetrec.exe` only records. Nothing is transcribed by itself.

```powershell
cd transcriber
uv sync                      # once: Python 3.11, PyTorch (CPU), models' runtimes
uv run python -m app         # serves http://127.0.0.1:8765/ and opens it
```

The page lists the recordings in MeetRec's `output_dir`, newest first, and
shows the newest transcript by default. Each row has a **Transcribe** /
**Re-transcribe** button beside it (and **Cancel** while it runs); click a row
to read its transcript, filtered to Both / Me / Others. The model is picked in
the header. One job runs at a time; more clicks queue up.

The mic channel and the system channel are transcribed separately, so every
line is labelled **Me** or **Others** without diarization. Each channel is cut
at pauses into pieces of at most 20 s; silence is skipped, and each piece
becomes one timestamped line.

| Model | Languages | Download | Runtime |
|---|---|---|---|
| [Underdog Bark 3.0](https://huggingface.co/ConwayResearch/Underdog-Bark-3.0) (default) | English | 163 MB | PyTorch, CPU (or CUDA) |
| [Cactus Whistle](https://huggingface.co/Cactus-Compute/whistle) | en, de, fr, es, it, nl, pl | 17 MB | `cactus-needle` native CPU engine |

Bark is a fine-tune of NVIDIA Parakeet TDT 0.6B v3 and the more accurate of
the two. Weights are downloaded on the first job at pinned revisions; the first
Bark job also unpacks the weights (about a minute), after which the model stays
loaded while the server runs. Whistle's anonymous usage telemetry is turned off
(`NEEDLE_TELEMETRY=0`).

**Settings**: `transcriber\settings.toml` (recordings folder, default model,
Bark device, Whistle language, cache folder, port). Models are cached in
`%LOCALAPPDATA%\MeetRec\transcriber` (plus the Whistle engine DLL in
`~\.cache\cactus-needle`).

**Output**: `<name>.md` next to the recording, same base name
(`2026-10-19_1430_Teams.opus` -> `2026-10-19_1430_Teams.md`); Re-transcribe
replaces it. A recording counts as transcribed when its `.md` exists.

```markdown
# Transcript: 2026-10-19 14:30 · Teams
- **Recording:** `2026-10-19_1430_Teams.opus`
…
**[00:01:23] Me:** Hello everyone.

**[00:01:25] Others:** Hi, can you hear me?
```

**Tests**: `cd transcriber; uv run python -m unittest discover -s tests -t .`

**Model licences**: Bark is CC-BY-4.0 (Conway Research, derived from NVIDIA
Parakeet TDT 0.6B v3; see `transcriber\app\BARK_NOTICE`), Whistle is
Apache-2.0 (Cactus Compute). Each transcript names the model it came from.

## Output

```
<output_dir>/2026/10/2026-10-19_1430_Teams.opus   Ogg Opus, 48 kHz
<output_dir>/2026/10/2026-10-19_1430_Teams.json   sidecar
```

- While recording, files are named `*.opus.part` / `*.json.part`. They are
  renamed when the recording closes, so a watcher only ever sees finished files.
- Stereo files use two **uncoupled** Opus streams (channel mapping family 1).
  The encoder never blends left and right. Windows Media Player can't decode
  this layout and plays only the mic, so it sounds muffled and the others are
  missing. Use the Recordings tab, VLC or ffmpeg instead.
- An Ogg page is flushed every second. After a crash, the leftover `.part` files
  are finalized on the next start with `"status": "recovered"`.
- Auto recordings shorter than `min_duration_secs` (default 30 s) are discarded.

Sidecar example:

```json
{
  "id": "b3a2b377-…",
  "file": "2026-10-19_1430_Teams.opus",
  "start": "2026-10-19T14:30:02+05:30",
  "end": "2026-10-19T15:01:40+05:30",
  "duration_secs": 1898.0,
  "trigger": "auto",
  "apps": ["Teams"],
  "devices": { "mic": "Headset Microphone", "system": "Speakers" },
  "channels": { "left": "mic", "right": "system" },
  "pauses": [{ "at_secs": 600.0, "start": "…", "end": "…" }],
  "markers": [{ "at_secs": 912.3, "time": "…" }],
  "status": "complete"
}
```

To split the channels for transcription:
`ffmpeg -i in.opus -af "pan=mono|c0=c0" me.wav` (use `c1` for the others).

## Config

`%APPDATA%\MeetRec\config.toml` (override the path with `MEETREC_CONFIG`). All
fields are optional:

```toml
capture_mode = "both"          # both | mic | system
mic_device = ""                # endpoint id; empty = Windows default
system_device = ""             # speakers/headset to record "others" from; empty = Windows default
output_dir = "C:\\Users\\me\\Documents\\MeetRec"
bitrate_kbps = 24              # per channel
auto_record = true
trigger_apps = ["Teams", "Zoom", "Webex", "Slack", "Chrome", "Edge", "Firefox"]
release_grace_secs = 10
min_duration_secs = 30
start_with_windows = false

[hotkeys]
start_stop = "ctrl+alt+R"
pause = "ctrl+alt+P"
split = "ctrl+alt+S"
marker = "ctrl+alt+M"

```

Logs go to `%APPDATA%\MeetRec\logs` (set `MEETREC_LOG=debug` for more detail).

## How it works

- **Capture**: WASAPI shared mode, polling. The mic comes from the capture endpoint;
  system audio comes from a render endpoint in loopback. Both default to the Windows
  default device. A meeting app set to a specific headset doesn't change the Windows
  default, so pick the same devices in Settings. Windows converts
  both to 48 kHz mono float. Stalls are filled with silence, because loopback is
  silent while nothing plays. A stream reopens on the new default device when the
  default changes or a device is unplugged, and keeps writing to the same file.
- **Mixer**: the mic clock is the master. System audio sits in a small jitter
  buffer: samples are dropped when it runs fast and silence is inserted when it
  starves.
- **Detection**: every 2 s the app reads the Windows privacy consent store
  (`HKCU\…\CapabilityAccessManager\ConsentStore\microphone`). An app is using the
  mic when `LastUsedTimeStart > 0` and `LastUsedTimeStop == 0`.
- **Controller**: a pure state machine (`src/controller.rs`) drives everything.
  The tray loop feeds it inputs and carries out its actions.
- Closes the recording on sleep and re-checks mic usage on wake.

## Building

Requires Rust (MSVC toolchain), the Visual Studio C++ build tools and CMake, which
builds libopus statically. `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM`
so libopus builds with CMake 4.

`build.ps1` finds the CMake bundled with the VS build tools, checks that MeetRec
isn't locking the exe, and builds (`-Dev` debug build, `-Test` run unit tests first,
`-Start` launch the tray afterwards, `-Force` kill a running MeetRec):

```
.\build.ps1 -Test -Start     # target\release\meetrec.exe
cargo build --release        # same, if CMake is already on PATH
cargo test --lib             # unit tests
cargo test --lib -- --ignored  # + tests that need real audio devices
meetrec.exe --record-test 60 out.opus   # 60 s capture spike, no tray
```

### Updating after a code change

Windows locks the exe while the tray is running, so quit it first with tray menu
→ **Quit**. That also closes any recording in progress cleanly. The settings
window runs from the same exe, so changes to it also need a rebuild.

```powershell
# 1. tray menu → Quit   (fallback: taskkill /f /im meetrec.exe; an open
#                        recording is then finalized as "recovered" on next start)
.\build.ps1 -Start                          # LTO build: needs plenty of RAM, takes a few minutes; then starts the tray
```

The "start with Windows" entry points at `target\release\meetrec.exe`, so it
picks up the new build automatically. Changing a default in `src/config.rs`
doesn't change an existing `config.toml`, which keeps the value it already has.
Edit that value in Settings.
