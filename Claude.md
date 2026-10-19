# MeetRec — meeting recorder (Rust, Windows tray app)

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

## Build

- Rust stable (MSVC toolchain), VS C++ build tools, CMake (VS 18 BuildTools
  ships one under `Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`).
- `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so libopus
  (built statically by `audiopus_sys`) configures under CMake 4.
- `cargo build --release` → `target\release\meetrec.exe` (~8.5 MB).
- `build.rs` generates the exe icon and embeds version info + a DPI-aware
  manifest (`assets/meetrec.manifest`) via `winresource`.

## Run modes (one exe)

- `meetrec.exe` — tray process (default). Single instance via the
  `Local\MeetRec.Tray` mutex. Never loads the GUI stack.
- `meetrec.exe --settings` / `--recordings` — egui window (separate
  short-lived process) with a Settings tab (edits `config.toml`, the tray
  hot-reloads it) and a Recordings tab (lists and plays recordings).
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
```

| File | Role |
|------|------|
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
| `src/tray.rs`, `src/hotkeys.rs` | tray icon/menu (`tray-icon`/`muda`), global hotkeys |
| `src/config.rs` | `config.toml` (serde defaults, sanitizing, mtime watcher) |
| `src/settings_ui.rs` | egui window: tab bar + Settings tab |
| `src/recordings_ui.rs`, `src/recordings.rs` | Recordings tab; listing `output_dir` with sidecars |
| `src/playback/` | `reader.rs` Ogg Opus decoder (any mapping, page index, sample-accurate seek), `mix.rs` L/R → both ears (Both / Me / Others), `player.rs` `Transport` (testable) + WASAPI render thread |
| `src/autostart.rs` | HKCU `Run` key |
| `src/power.rs` | suspend/resume notifications (closes recording before sleep) |

## Output

`<output_dir>/YYYY/MM/YYYY-MM-DD_HHMM_<App>.opus` + `.json` sidecar
(`status: complete | recovered`, trigger, apps, devices, channel map, pauses,
markers). Files are `.part` while recording.

## Config

`%APPDATA%\MeetRec\config.toml` (override with `MEETREC_CONFIG`): capture_mode,
mic_device, output_dir, bitrate_kbps, auto_record, trigger_apps,
release_grace_secs (10), min_duration_secs (30), hotkeys, start_with_windows.
Logs: `%APPDATA%\MeetRec\logs` (`MEETREC_LOG=debug` for more).

## Known open issues

- **libopus is compiled unoptimized** — the `cmake` crate strips `/O*` flags, so
  recording costs ~16% of one core (idle 0.2%). `CFLAGS` won't help (filtered);
  candidates: `CL=/O2` in `.cargo/config.toml` `[env]`, or `CMAKE_GENERATOR=Ninja`.
  Unverified.
- Windows Media Player (Web Media Extensions' Opus decoder) plays only the
  first stream of the family-1 stereo files, i.e. just the mic. That's why the
  Recordings tab decodes in-process.
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
