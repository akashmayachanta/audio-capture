# AudCap

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
- **Dictation**: hold Right Alt, speak, release: the text is typed where your
  cursor is (see [Dictation](#dictation)).

## Using it

Run `audcap.exe`. The tray icon shows the state:

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

A toast appears when an auto recording starts. **Recordings…** and
**Settings…** in the tray menu open the AudCap window (`audcap.exe
--recordings` / `--settings`, one window at a time). It follows Windows' light
or dark mode.

**Recordings** lists your recordings grouped by day, newest first. Select one to
see it on the right:

- **Player**: play/pause, seek bar, volume. Both channels play in both ears;
  **Listen** picks Both, Me only (the mic) or Others only (system audio).
- **Transcript** (when the recording has one): lines labelled Me / Others,
  filtered with **Show**. Click a line's timestamp to play from there; the line
  being played is highlighted.
- **Transcribe** / **Cancel** / progress beside each recording while the
  transcriber is running (see [Transcription](#transcription)). Without it,
  everything else, including existing transcripts, still works.

**Settings** edits `config.toml` (capture, devices, folder, quality,
auto-record apps and timing, hotkeys, dictation keys, start with Windows); **Save** writes it and
the tray picks up the change live. Its **Transcription** card loads models
while the transcriber is running.

## Dictation

Speak into any text field, in any app.

| Gesture | What happens |
|---|---|
| Hold **Right Alt**, speak, release | the text is pasted where the cursor is |
| Double-tap **Right Alt** | hands-free: keeps listening until you double-tap again |
| **Esc** | cancels (while listening or transcribing) |

A small round microphone badge at the bottom of the screen shows the state:
teal = listening (its halo grows with your voice), amber = the speech service is
starting, indigo = transcribing (a light runs round the rim), red = error. Messages such as "No speech heard" arrive as toasts.
Right Alt with another key (AltGr combinations) types normally, and a single
quick tap does nothing.

- **Requirements**: the transcriber (see [Transcription](#transcription)) with
  a model loaded. If it is not running, the first dictation starts it and
  waits up to 90 s (with the model loaded last); dictation never stops it. The
  tray's **Speech service** submenu does it by hand: **Start with** a model
  (which then becomes the default), switch the running service to another model
  (the ticked one is active), or **Stop** it. A dictation cuts in ahead of a running
  meeting transcription, which then continues.
- **Keys**: **Settings → Dictation** picks the key for each gesture (or Off).
  **Detect…** binds the next key you press. Keys a keyboard handles by itself
  (Fn on most laptops) are not reported to Windows and can't be used.
- **While you speak**: the audio is cut into phrases at pauses (about 0.6 s)
  and each phrase is transcribed straight away, so little is left to do when
  you stop. In hands-free mode each phrase is typed as soon as it is ready;
  choose **Settings → Dictation → Paste the text → When I finish** to get
  everything at once instead (Esc then discards all of it; with "While I
  speak", what is already typed stays). Hold to talk always pastes on release,
  because the held key would get in the way of the paste.
- **Pasting**: the text goes through the clipboard (Ctrl+V); the text that was
  there is put back half a second later and the dictation is kept out of the
  clipboard history. When the clipboard holds an image or files the text is
  typed instead, so nothing is lost. In a window running as administrator the
  text stays on the clipboard and a toast says so.
- **Audio**: each dictation is kept as a mono Opus clip in
  `<output_dir>\dictation\YYYY\MM\` (turn off **Keep the audio** in Settings
  to delete them). These clips don't show in the Recordings list.
- Listening stops by itself after `max_secs` (default 300 s) and transcribes
  what it has. Dictation works during a meeting recording and doesn't trigger
  auto-record.

## Transcription

Manual, on-device speech-to-text in an optional local API
(`backend\transcriber\`, Python + FastAPI, set up with
[uv](https://docs.astral.sh/uv/)). It has no page of its own: the AudCap window
is its UI. `audcap.exe` only records, and nothing is transcribed by itself.

Start and stop it with the **Start** / **Stop** button in **Settings →
Transcription** (also on the Recordings page while it is offline). `audcap.exe`
runs `uv run python -m app` in the transcriber folder, hidden, logging to
`%APPDATA%\AudCap\logs\transcriber.log`; the server keeps running after the
window closes. The folder is found next to the exe or in the repo
(`backend\transcriber`); set `transcriber_dir` in `config.toml` otherwise. The
first start installs the Python environment and can take a few minutes. By hand:

```powershell
cd backend\transcriber
uv sync                      # once: Python 3.11, PyTorch (CPU), OpenVINO, models' runtimes
uv run python -m app         # API on http://127.0.0.1:8765/
```

(or `backend\transcriber\run.bat`). While it runs, the AudCap window shows **Transcribe**
beside each recording (**Redo** on one that has a transcript, **Cancel** while
it runs). One job runs at a time; more clicks queue up. The transcriber must
read the same folder AudCap saves to; if its `recordings_dir` points
elsewhere, the window says so and hides Transcribe.

**Model**: the window's header shows which model is loaded (or loading, or
failed). **Settings → Transcription** lists both models with a **Load** button
each. Only one model is in memory and transcriptions use it. The transcriber
loads a model as soon as it starts: the one you loaded last, or `model` from
`settings.toml` the first time.

The mic channel and the system channel are transcribed separately, so every
line is labelled **Me** or **Others** without diarization. Each channel is cut
at pauses into pieces of at most 20 s; silence is skipped, and each piece
becomes one timestamped line.

| Model | Languages | Download | Runtime |
|---|---|---|---|
| [Cactus Whistle](https://huggingface.co/Cactus-Compute/whistle) (default) | en, de, fr, es, it, nl, pl | 17 MB | `cactus-needle` native CPU engine |
| [Underdog Bark 3.0](https://huggingface.co/ConwayResearch/Underdog-Bark-3.0) | English | 163 MB | PyTorch on the CPU (or CUDA); on Intel graphics the encoder runs through OpenVINO |

Bark is a fine-tune of NVIDIA Parakeet TDT 0.6B v3 and the more accurate of
the two. Weights are downloaded the first time a model loads, at pinned
revisions. The first Bark load also unpacks its weights and, with Intel
graphics, converts and compiles the encoder for the GPU (two to three minutes
in all, once); the results are kept in the cache folder (about 1.3 GB, or
3.7 GB with the GPU files), so later loads take well under a minute. Whistle's
anonymous usage telemetry is turned off (`NEEDLE_TELEMETRY=0`), and so is
OpenVINO's.

**Bark device** (`device` in `settings.toml`): `auto` (default) uses Intel
graphics when OpenVINO finds a GPU and falls back to the CPU if that fails;
`cpu`, `gpu` and `cuda` force one. On a Core Ultra 7 155U the integrated GPU
ran Bark at 10-13x real time against 3-4x on the CPU, with 2-3 words in 400
differing (punctuation).

**Speed / cores**: one Whistle engine uses only about 2 cores, so the
transcriber runs several Whistle processes side by side, each with its own copy
of the 17 MB model (`workers` in `settings.toml`; 0 = auto, a third of the
logical cores, at most 4). On a Core Ultra 7 155U a 7-minute call went from
2.7x to 6.6x real time; 6 processes were slower than 4. Bark is already
multi-threaded through PyTorch (one thread per physical core by default).

**Settings**: `backend\transcriber\settings.toml` (recordings folder, default model,
Bark device, Whistle language, Whistle worker processes, cache folder, port). Models are cached in
`%LOCALAPPDATA%\AudCap\transcriber` (plus the Whistle engine DLL in
`~\.cache\cactus-needle`). `transcriber.db` there (SQLite) holds the model
list the Settings dialog shows (`models` table; set `enabled = 0` to hide one)
and the model loaded last (`prefs`).

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

**Model licences**: Bark is CC-BY-4.0 (Conway Research, derived from NVIDIA
Parakeet TDT 0.6B v3; see `backend\transcriber\app\BARK_NOTICE`), Whistle is
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
  missing. Use the AudCap window, VLC or ffmpeg instead.
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

`%APPDATA%\AudCap\config.toml` (override the path with `AUDCAP_CONFIG`). All
fields are optional:

```toml
capture_mode = "both"          # both | mic | system
mic_device = ""                # endpoint id; empty = Windows default
system_device = ""             # speakers/headset to record "others" from; empty = Windows default
output_dir = "C:\\Users\\me\\Documents\\AudCap"
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

[dictation]
enabled = true
hold_key = "RightAlt"          # hold to talk; "" = off. RightAlt, RightCtrl, CapsLock, F13..F24, vk:0x7B, ...
toggle_key = "RightAlt"        # double-tap for hands-free; "" = off
keep_audio = true              # keep clips under <output_dir>\dictation
max_secs = 300                 # listening stops by itself (5..600)
paste_mode = "live"            # live = hands-free types each phrase as it is ready; end = all at once
```

Logs go to `%APPDATA%\AudCap\logs` (set `AUDCAP_LOG=debug` for more detail).

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
- **Controller**: a pure state machine (`backend/recorder/src/controller.rs`)
  drives everything. The tray loop feeds it inputs and carries out its actions.
- **Window**: a separate short-lived process (`--settings` / `--recordings`), so
  the tray never loads a GUI stack. It shows the Svelte app (`frontend/`, Svelte 5
  + Tailwind CSS, embedded in the exe) in WebView2. The page talks to the exe over
  a custom protocol (`http://audcap.localhost/api/...`: config, devices,
  recordings, audio with seeking, transcripts) and, when it's running, to the
  transcriber API on `127.0.0.1:8765`.
- Closes the recording on sleep and re-checks mic usage on wake.

## Layout

```
frontend/               Svelte app (the window's UI); builds to frontend/dist
backend/recorder/       Rust crate -> audcap.exe (tray, capture, window host)
backend/transcriber/    optional transcription API (Python, uv)
build.ps1               builds the frontend, then the exe
```

## Building

Requires Rust (MSVC toolchain), the MSVC C++ build tools, CMake (builds
libopus statically) and Node.js (builds the UI). `.cargo/config.toml` sets
`CMAKE_POLICY_VERSION_MINIMUM` so libopus builds with CMake 4. Without CMake you
can point `LIBOPUS_LIB_DIR` at a folder whose `lib\` holds a prebuilt `opus.lib`.

Without admin rights: install CMake and Ninja per user with
`uv tool install cmake` and `uv tool install ninja`, and use a portable MSVC
(`cl.exe`, `link.exe`, `rc.exe` on `PATH`, `INCLUDE`/`LIB` set). When no Visual
Studio install is registered, `build.ps1` builds libopus with the Ninja
generator and the `cl.exe` on `PATH` (adding `/FS` to `CFLAGS`), and sets
`RC_PATH` from the `rc.exe` on `PATH` for the exe's resources.

`build.ps1` builds the frontend (`npm ci` when needed, `npm run build` only when the UI sources changed), finds CMake
(on `PATH`, else the one bundled with the VS build tools), checks that AudCap isn't locking the exe,
and builds (`-Dev` debug build, `-Test` run frontend and Rust unit tests first,
`-Start` launch the tray afterwards, `-Force` kill a running AudCap):

```
.\build.ps1 -Test -Start     # backend\recorder\target\x86_64-pc-windows-msvc\release\audcap.exe
.\copy-release.bat           # copy that exe to C:\Drive\Software\Apps\audio-capture and start it there
cd frontend; npm test        # UI unit tests (Vitest)
cd frontend; npm run dev     # UI dev server (API calls need the exe, so mostly for styling)
cd backend\recorder
cargo build --release        # exe only; embeds frontend\dist if it was built (else a placeholder page)
cargo test --lib             # unit tests
cargo test --lib -- --ignored  # + tests that need real audio devices
audcap.exe --record-test 60 out.opus   # 60 s capture spike, no tray
```

### Updating after a code change

Windows locks the exe while the tray is running, so quit it first with tray menu
→ **Quit**. That also closes any recording in progress cleanly. The settings
window runs from the same exe, so changes to it also need a rebuild.

```powershell
# 1. tray menu → Quit   (fallback: taskkill /f /im audcap.exe; an open
#                        recording is then finalized as "recovered" on next start)
.\build.ps1 -Start                          # LTO build: needs plenty of RAM, takes a few minutes; then starts the tray
```

The build always targets `x86_64-pc-windows-msvc` (`.cargo\config.toml`), so
the exe is in `backend\recorder\target\x86_64-pc-windows-msvc\release\`. For
daily use, `copy-release.bat` installs it in `C:\Drive\Software\Apps\audio-capture`:
it stops the transcriber and every running AudCap (an open recording is then
finalized as "recovered"; quit from the tray first to avoid that), deletes the
old exe there and the obsolete pre-target build folders (`target\`,
`backend\recorder\target\release`, `…\debug`), copies the new exe, links
`transcriber\` there to the repo's `backend\transcriber` (so **Start** in the
window finds it), and starts the tray. With **Start with Windows** on, the tray
points the sign-in entry at whichever exe it runs from on every start, so an
entry left by an old build folder is corrected. Changing a default in `src/config.rs`
doesn't change an existing `config.toml`, which keeps the value it already has.
Edit that value in Settings.
