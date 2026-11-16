# MeetRec — meeting recorder (Rust tray app + Svelte window + optional transcriber)

Records **microphone + system audio** to Ogg Opus files for a separate
transcription/notes app. Mic goes on the **left** channel, system audio
(other participants) on the **right**, so "me" vs "others" needs no diarization.
Records automatically when an allow-listed app (Teams, Zoom, Webex, Slack,
Chrome, Edge, Firefox, or a custom exe name) starts using the mic, and manually
via tray menu / hotkeys. See `README.md` for user-facing docs and
`docs/plans/meetrec-plan.md` for the original design.

## Instructions

- **Never commit to GIT.** Do not run `git commit`, create branches, push, or
  rewrite history. Leave all changes uncommitted; the user commits themselves.
- Write tests first for logic changes (`cargo test --lib`); keep the controller
  pure and testable.
- Release builds use LTO and need a lot of RAM; prefer debug builds and
  `cargo test --lib` while iterating.
- Tests that need real audio hardware are `#[ignore]`; run them with
  `cargo test --lib -- --ignored`.
- For live runs, point the app at a scratch config with `MEETREC_CONFIG` so the
  user's `%APPDATA%\MeetRec\config.toml` and recordings folder are not touched.
- Don't write test cases for 'transcriber' - python app
- Frontend logic lives in pure `frontend/src/lib/*.ts` with Vitest tests
  (`cd frontend; npm test`); `npm run check` must stay at 0 errors/warnings.

## Layout

```
frontend/               Svelte 5 + TypeScript + Tailwind CSS v4 (Vite) -> frontend/dist
backend/recorder/       Rust crate -> meetrec.exe (Cargo.toml, build.rs, src/, assets/)
backend/transcriber/    optional transcription API (Python, uv)
.cargo/config.toml      stays at the repo root (cargo finds it from backend/recorder)
build.ps1               npm build, then cargo build in backend/recorder
```

## Build

- Rust stable (MSVC toolchain), VS C++ build tools, CMake (VS 18 BuildTools
  ships one under `Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`),
  Node.js (npm) for the frontend. Without CMake, `LIBOPUS_LIB_DIR=<dir>` (with
  `<dir>\lib\opus.lib`) makes `audiopus_sys` link a prebuilt libopus.
- No-admin setup (the user's machine): portable MSVC on `PATH` (+ `INCLUDE`/`LIB`),
  `uv tool install cmake` / `uv tool install ninja`. With no registered VS,
  `build.ps1` sets `CMAKE_GENERATOR=Ninja` and `CFLAGS=/FS` (C1041 otherwise),
  and `RC_PATH` from the `rc.exe` on PATH (`winresource` only checks the registry).
  `Cargo.toml` builds `audiopus_sys` at opt-level 2 in dev: at opt-level 0 the
  cmake crate configures CMake "Debug" → `/MDd` → unresolved `__imp__CrtDbgReportW`.
  After changing any of these env vars, `cargo clean -p audiopus_sys`.
- `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so libopus
  (built statically by `audiopus_sys`) configures under CMake 4.
- `.cargo/config.toml` also sets `[build] target = "x86_64-pc-windows-msvc"`, so
  output is under `target\x86_64-pc-windows-msvc\{debug,release}\`.
- In `backend/recorder`: `cargo build --release` → `target\x86_64-pc-windows-msvc\release\meetrec.exe`.
  `copy-release.bat` (repo root) installs it in `C:\Drive\Software\Apps\audio-capture`:
  stops the transcriber (`/api/shutdown`, else kills the uv/python tree) and all
  MeetRec processes, deletes the old exe + obsolete `target\`,
  `backend\recorder\target\{release,debug}`, copies, junctions `transcriber\` to
  the repo's, starts the tray.
  `.\build.ps1` wraps it: `npm ci` (when needed) + `npm run build` in `frontend/` (skipped when nothing in `frontend/` or `src/` is newer than `dist/index.html`),
  puts CMake on PATH (from PATH, or VS-bundled via vswhere), refuses if MeetRec runs from
  the output exe (`-Dev`, `-Test` = Vitest + `cargo test --lib`, `-Start`, `-Force`).
- `build.rs` generates the exe icon, embeds version info + a DPI-aware manifest
  (`assets/meetrec.manifest`) via `winresource`, and stages `frontend/dist` into
  `$OUT_DIR/frontend` for `include_dir!` — or a one-line placeholder page with a
  cargo warning when `dist` is missing, so `cargo test --lib` needs no npm.
- From Git Bash, `/usr/bin/link` shadows MSVC `link.exe`; run cargo from PowerShell.

## Run modes (one exe)

- `meetrec.exe` — tray process (default). Single instance via the
  `Local\MeetRec.Tray` mutex. Never loads the GUI stack.
- `meetrec.exe --settings` / `--recordings` — the window (separate short-lived
  process, `Local\MeetRec.Settings` mutex): tao + wry (WebView2) showing the
  Svelte app at `http://meetrec.localhost/#/settings` or `#/recordings`.
  Both pages stay mounted (tabs only hide them). Navigation, new windows and
  file drops can't leave the app origin. WebView2 data in
  `%LOCALAPPDATA%\MeetRec\webview`; logs go to `logs\meetrec-window.*.log`. For live checks set
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` and drive
  it over CDP (PrintWindow can't capture WebView2).
- `meetrec.exe --record-test [secs] [out.opus]` — capture spike without the tray.

## Architecture

```
mic capture thread ─┐
                    ├─ ring buffers ─▶ engine thread (mixer → session/encoder) ─▶ .opus.part
loopback thread ────┘
mic-usage detector (2 s) ─┐
tray menu / hotkeys ──────┼─▶ app loop (Win32 message pump) ─▶ controller ─▶ actions ─▶ recorder
config watcher / power / ─┘
device-change events

window process: WebView2 page ──meetrec:// custom protocol──▶ web_api (config, devices, recordings, audio, transcripts)
                              └──fetch 127.0.0.1:8765──────▶ transcriber API (jobs, models), optional
```

Paths below are under `backend/recorder/`.

| File | Role |
| ------ | ------ |
| `src/main.rs` | arg dispatch, file logging, single-instance mutex |
| `src/app.rs` | tray process loop: drains menu/hotkey/detector/device/power events, feeds the controller, executes actions, config hot-reload, toasts |
| `src/controller.rs` | **pure** state machine `Idle / Armed / Recording / Paused`; auto rules (grace timer, min duration for auto sessions) |
| `src/recorder.rs` | one recording: capture streams + engine thread; split/pause/marker/stop commands |
| `src/session.rs` | file lifecycle: `.opus.part`/`.json.part` → rename on close, sidecar JSON, crash recovery of leftover `.part` files |
| `src/audio/capture.rs` | WASAPI shared-mode polling capture (mic = capture endpoint, system = render endpoint loopback); autoconvert to 48 kHz mono f32; silence-fills real stalls only; reopens on failure/restart |
| `src/audio/mixer.rs` | 20 ms frames; mic is master clock; system audio jitter buffer with drift correction; L = mic, R = system |
| `src/audio/convert.rs` | fallback decode/downmix/resample (rubato) when a driver refuses autoconvert |
| `src/audio/device.rs` | endpoint lookup (falls back to default if chosen mic is not active), default-device change watcher |
| `src/encode/opus_ogg.rs` | libopus multistream encoder (2 uncoupled mono streams, mapping family 1; mono = family 0) + Ogg writer, page flush every 1 s |
| `src/detect/mic_usage.rs` | polls `HKCU\…\CapabilityAccessManager\ConsentStore\microphone`; in use = `LastUsedTimeStart > 0 && LastUsedTimeStop == 0`; excludes our own exe |
| `src/tray.rs`, `src/hotkeys.rs` | tray icon/menu (`tray-icon`/`muda`; menu on right-click only, left double-click opens Recordings), global hotkeys |
| `src/icon.rs` | the MeetRec mark (rounded square + white wave bars), dependency-free; tray (state colours), window icon, and `build.rs` (exe icon, via `#[path]`); the header SVG in `App.svelte` copies it |
| `src/transcriber.rs` | `find_dir` (config `transcriber_dir`, else `transcriber/` or `backend/transcriber/` up from the exe); `Launcher` spawns `uv run python -m app` hidden (log `logs/transcriber.log`), `status()` = running / exit code + log tail |
| `src/config.rs` | `config.toml` (serde defaults, sanitizing, mtime watcher) |
| `src/window.rs` | the window: tao event loop, wry WebView, async `meetrec` custom protocol → `web_api::handle` on a thread per request; `WindowHost` (devices on an MTA thread, rfd folder picker, Explorer); message box if WebView2 fails |
| `src/web_api/mod.rs` | **pure** router `handle(&dyn Host, &Request) -> Response`: `GET/PUT /api/config` (sanitized save, `{config, known_apps}`), `GET /api/devices`, `GET /api/recordings` (`{output_dir, recordings}`), `GET /api/transcript?id=` (raw `.md`), `GET /api/audio?id=` (Range → 206, open-ended capped at 2 MiB, bad → 416), `POST /api/open-folder`, `POST /api/pick-folder`, `GET /api/transcriber` (launch status + `dir`), `POST /api/transcriber/start`; other paths → assets |
| `src/web_api/files.rs` | id (path relative to `output_dir`, `/`-separated) → `.opus` inside the root only; `parse_range`; `speakers` from the sidecar channel map; `RecordingJson` |
| `src/web_api/assets.rs` | embedded `frontend/dist` (`include_dir!`), content types |
| `src/recordings.rs` | listing `output_dir` with sidecars and `.md` presence |
| `src/autostart.rs` | HKCU `Run` key (path of the running tray exe) |
| `src/power.rs` | suspend/resume notifications (closes recording before sleep) |

## Output

`<output_dir>/YYYY/MM/YYYY-MM-DD_HHMM_<App>.opus` + `.json` sidecar
(`status: complete | recovered`, trigger, apps, devices, channel map, pauses,
markers). Files are `.part` while recording.

## Config

`%APPDATA%\MeetRec\config.toml` (override with `MEETREC_CONFIG`): capture_mode,
mic_device, system_device (render endpoint for loopback), output_dir, bitrate_kbps, auto_record, trigger_apps,
release_grace_secs (10), min_duration_secs (30), hotkeys, start_with_windows,
transcriber_dir (empty = auto).
Logs: `%APPDATA%\MeetRec\logs` (`MEETREC_LOG=debug` for more). A leftover
`[transcription]` section from older builds is ignored.

## Frontend (`frontend/`)

One app for both pages; hash router in `App.svelte` (top bar: tabs + transcriber
status). Tokens are CSS variables in `src/app.css` (light + dark via
`prefers-color-scheme`) mapped into Tailwind with `@theme inline` (`bg-panel`,
`text-muted`, `text-me`, …); shared classes `.btn`, `.icon-btn`, `.input`,
`.hint`, `.range` live there too. CSP in `index.html` allows only self +
`http://127.0.0.1:8765`.

| File | Role |
| ------ | ------ |
| `src/lib/api.ts` | `local` (same origin) and `transcriber` clients, `Config` type, `ApiError` |
| `src/lib/transcriber.svelte.ts` | pure `reduce` (offline keeps last state; `finishedJobIds`) + `createTranscriber()`: one request at a time, next poll 2 s after the previous reply |
| `src/lib/service.ts` | pure Start/Stop state (`settle`, `phase`): starting until the API answers or the launched process exits (failure = exit code + log tail), stopping until it goes away (15 s timeout); wired into `createTranscriber` (`service`, `startService`, `stopService`) and `components/ServiceButton.svelte` |
| `src/lib/numbers.ts` | `clampInt` for Settings number fields (same ranges as `Config::sanitized`) |
| `src/lib/transcript.ts` | parses the transcript `.md` (same regexes as `markdown.py`; the only reader of the format) |
| `src/lib/{format,apps,recordings,listen}.ts` | times, day groups, trigger-app split/join, merge recordings + jobs, `sameFolder`, Listen gains |
| `src/routes/Recordings.svelte` | list + detail; hints (offline / folder mismatch / no model); refresh on focus and when a job finishes |
| `src/routes/Settings.svelte` | config form with Save/Discard bar when dirty; Transcription card (Start/Stop, Load model) |
| `src/components/Player.svelte` | `<audio>` → Web Audio splitter → per-channel gain → both ears; exports `seek()`/`play()` |
| `src/components/TranscriptView.svelte` | Show filter, timestamp seek, playing line highlight + scroll |

## Transcription (`backend/transcriber/`, Python, separate from the exe)

The Rust exe doesn't transcribe. `backend/transcriber/` is a uv project (Python
3.11, pinned deps, torch from the PyTorch CPU index) serving an API only (the
window is its UI): `uv run python -m app` → `http://127.0.0.1:8765/`. The window's
Start button runs that (see `src/transcriber.rs`); Stop calls `POST /api/shutdown`. Settings in
`backend/transcriber/settings.toml` (`MEETREC_TRANSCRIBER_SETTINGS` overrides the path;
use it with a scratch `recordings_dir`/`cache_dir` for live runs). Recordings dir
defaults to `output_dir` from MeetRec's config.toml; the window disables
Transcribe when the two folders differ.

| File | Role |
| ------ | ------ |
| `app/server.py` | `build()` wires store + engines and starts loading the startup model; routes: `/api/state`, `POST /api/load`, `POST /api/transcribe`, `POST /api/cancel`, `POST /api/shutdown` (`__main__` sets uvicorn `should_exit`, closes the engine, `os._exit`); Host-header check (DNS rebinding); CORS for `http://meetrec.localhost` only |
| `app/jobs.py` | one worker thread, queue, cancel; decode → chunk → engine → `<name>.md` |
| `app/engines.py` | `BarkEngine` (Underdog Bark 3.0 via transformers `ParakeetForTDT`, batches of 4) and `WhistleEngine` (`cactus-needle`, telemetry off; one model per process and ~2 cores each, so `workers` > 1 = a `ProcessPoolExecutor` of Whistles, `close()` terminates them); pinned HF revisions; `CATALOG`; `EngineCache` holds one model: `load_async` + `status()` for the header, jobs `get()` the active one |
| `app/store.py` | SQLite `transcriber.db` in the cache folder: `models` (list shown in Settings, order from `CATALOG`; `enabled` hides one) and `prefs.model` (loaded last, reloaded at startup) |
| `app/bark_loader.py` | vendored `.bark` unpacker (CC-BY-4.0, see `BARK_NOTICE`); never exec downloaded code |
| `app/audio.py` | PyAV decode of the Ogg Opus (family 1 multistream works) → 16 kHz per speaker |
| `app/chunking.py` | pure: energy VAD, pieces ≤ 20 s cut at pauses / quietest frame |
| `app/markdown.py`, `app/recordings.py` | transcript format (writer; `frontend/src/lib/transcript.ts` reads it); listing/sidecars/path checks |

- Measured (155U, CPU): Bark ~4x real time on speech (silence is skipped), model
  unpack 1-2 min per server start (then kept warm); Whistle several times
  faster but less accurate (1 process 2.1-2.7x, 4 processes 6.6-7.1x; 6 were slower than 4); Whistle `language = "auto"` mislabels short noisy
  pieces, so the default is `en`.
- Old OpenVINO cache folders in `%LOCALAPPDATA%\MeetRec` (`venv`, `models`,
  `ov_cache`, `tmp`) are unused now.

## Known open issues

- **libopus is compiled unoptimized** — the `cmake` crate strips `/O*` flags, so
  recording costs ~16% of one core (idle 0.2%). `CFLAGS` won't help (filtered);
  candidates: `CL=/O2` in `.cargo/config.toml` `[env]`, or `CMAKE_GENERATOR=Ninja`.
  Unverified.
- Windows Media Player (Web Media Extensions' Opus decoder) plays only the
  first stream of the family-1 stereo files, i.e. just the mic. WebView2 (Edge)
  decodes both channels, so the window plays the files as-is.
- `audio::capture::tests::stream_stays_continuous_across_a_restart` (ignored)
  fails on the Intel SST mic: the first open took >1 s, and a rerun got 0.52 s
  of audio for 2 s. Suspected cause, unverified: `Clock::push` re-anchors when
  the first packet after a long open/restart arrives, before `fill_gap` pads
  the stall, so the gap is lost.
- Shutdown/sign-out mid-recording is not handled (no `WM_ENDSESSION`); the file
  is finalized as "recovered" on next start.
- Deferred minors: mixer slow-side drift only corrected by underrun; config
  writes not atomic; capture/detector failures spam the log; stale mic holders
  on resume; apps not carried across a split; `live_consent_store_is_readable`
  test reads the real registry.
