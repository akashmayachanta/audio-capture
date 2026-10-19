# MeetRec

A small Windows tray app that records meetings to files: **your microphone on the
left channel, system audio (everyone else) on the right**. A separate
transcription/notes app can then label "me" vs "others" without speaker
diarization.

- **Auto-record**: starts when an allow-listed app (Teams, Zoom, Webex, Slack,
  Chrome, Edge, Firefox, or any exe you add) starts using the microphone. Stops
  after the mic has been released for `release_grace_secs` (default 20 s).
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

## Output

```
<output_dir>/2026/10/2026-10-19_1430_Teams.opus   Ogg Opus, 48 kHz
<output_dir>/2026/10/2026-10-19_1430_Teams.json   sidecar
```

- While recording, files are named `*.opus.part` / `*.json.part`. They are
  renamed when the recording closes, so a watcher only ever sees finished files.
- Stereo files use two **uncoupled** Opus streams (channel mapping family 1).
  The encoder never blends left and right.
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
output_dir = "C:\\Users\\me\\Documents\\MeetRec"
bitrate_kbps = 24              # per channel
auto_record = true
trigger_apps = ["Teams", "Zoom", "Webex", "Slack", "Chrome", "Edge", "Firefox"]
release_grace_secs = 20
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
  system audio comes from the default render endpoint in loopback. Windows converts
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

```
cargo build --release        # target\release\meetrec.exe
cargo test --lib             # unit tests
cargo test --lib -- --ignored  # + tests that need real audio devices
meetrec.exe --record-test 60 out.opus   # 60 s capture spike, no tray
```
