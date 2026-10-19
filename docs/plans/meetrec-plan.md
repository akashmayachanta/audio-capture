# Meeting Recorder — Rust tray app (Windows)

Spec: this document (approved plan, 2026-10-19). Original design discussion lives in the
Claude Code session transcript; this file is the binding copy.

## Context
Lightweight Windows tray app that records **mic + system audio** to files for a separate
transcription/notes app. Records automatically when a meeting app (Teams, Zoom, browser…)
starts using the mic, plus manual start/stop/pause/split. Small settings window picks the
capture mode: mic only, system only, or both.

Decisions: **Rust**, **auto (mic-in-use) + manual** segmentation, **Ogg Opus with mic on the
left channel and system audio on the right** (lets the transcript app label "Me" vs
"Others" without diarization).

## Global Constraints
- One exe `meetrec.exe`: default = tray process (no GUI stack loaded, ~10–20 MB RAM);
  `--settings` = egui settings window that edits `config.toml`; tray reloads on mtime change.
- Threads: mic capture, loopback capture → mixer/encoder → `.opus.part`; mic-usage detector
  (2 s poll) → controller state machine ← tray menu / hotkeys / config reload.
- Controller is pure: states `Idle | Armed | Recording{trigger, app} | Paused`; inputs
  `MicInUse(app)`, `MicReleased`, `ManualStart/Stop/Pause/Resume/Split/Marker`,
  `DeviceChanged`, `ConfigChanged`, `Suspend/Resume`.
- Rules: auto Armed→Recording when allow-listed app takes mic; stop after mic released
  for `release_grace_secs` (default 20). Manual starts stop only manually. Split = close +
  reopen immediately. Pause keeps file, logs interval in sidecar. Sessions shorter than
  `min_duration_secs` (default 30) discarded. Suspend closes session; resume re-checks.
- Audio: WASAPI shared mode; mic = capture endpoint; system = default render endpoint in
  loopback, **polling** not event mode, fill gaps with silence, respect
  `AUDCLNT_BUFFERFLAGS_SILENT`. Downmix to mono f32; resample to 48 kHz with `rubato` only
  if needed. Device changes via `IMMNotificationClient`, restart stream, same file.
- Mixer: ring buffers per source, every 20 ms pull 960 frames each, zero-pad short source.
  Mic clock master; drop samples if loopback buffer grows past threshold, insert silence if
  it starves. L=mic, R=system; mono when only one source.
- Encoder: libopus via `audiopus_sys` static + `ogg` crate; multistream encoder, mapping
  family 1, 2 uncoupled mono streams (~24–32 kbps each, VOIP). Flush Ogg page ~every 1 s.
- Sessions: `<output_dir>/YYYY/MM/YYYY-MM-DD_HHMM_<App>.opus`; write `.opus.part`, rename on
  close; sidecar `.json` (id, start/end ISO-8601, duration, trigger, apps, device names,
  channel map, pauses, markers, status complete|recovered); recover leftover `.part` on start.
- Detection: poll HKCU `...\CapabilityAccessManager\ConsentStore\microphone` (packaged
  subkeys + `NonPackaged\<exe path with #>`); in use when `LastUsedTimeStart > 0 &&
  LastUsedTimeStop == 0`; exclude own exe; friendly names; allow-list from config.
- Tray: `tray-icon` + `muda` on Win32 loop, `global-hotkey`. Icons grey/blue/red/amber.
  Tooltip elapsed + app. Menu: Start/Stop, Pause/Resume, Split now, Add marker,
  ✓ Auto-record, Capture ▸ (Mic/System/Both), Open recordings folder, Settings…, Quit.
  Hotkeys Ctrl+Alt+R/P/S/M. Toast on auto-start (`tauri-winrt-notification`).
- Config `%APPDATA%\MeetRec\config.toml`: capture_mode, mic_device, output_dir,
  bitrate_kbps, auto_record, trigger_apps, release_grace_secs, min_duration_secs, hotkeys,
  start_with_windows.
- Misc: single-instance named mutex; autostart HKCU Run; `tracing` + rolling file;
  `#![windows_subsystem = "windows"]`; release `lto`, `opt-level="s"`, `strip`.

## Task 1: Spike — CLI records mic + loopback to dual-mono Opus
Cargo project, capture (`src/audio/capture.rs`, `device.rs`), mixer (`src/audio/mixer.rs`),
encoder (`src/encode/opus_ogg.rs`). A CLI mode records N seconds to an Ogg Opus file.
Verify libopus builds, L/R separated, works with nothing playing.
Tests: mixer padding/drift unit tests; Ogg writer round-trip (parse back pages, check
OpusHead channel mapping, granule monotonic). Test command: `cargo test`.

## Task 2: Session manager
`src/session.rs`: `.part` → rename, sidecar JSON, pause/resume/split, min-duration discard,
crash recovery of leftover `.part`. Tests: path naming, sidecar contents, recovery on temp
dir. Test command: `cargo test`.

## Task 3: Controller + tray + hotkeys (manual mode)
`src/controller.rs` pure state machine with unit tests; `src/tray.rs`, `src/hotkeys.rs`,
icons, single-instance mutex, logging. Test command: `cargo test`.

## Task 4: Mic-in-use detector → auto mode
`src/detect/mic_usage.rs` registry polling + parsing with fixture tests; controller auto
rules with grace. Test command: `cargo test`.

## Task 5: Settings process + config hot-reload + autostart + toasts
`src/config.rs`, `src/settings_ui.rs`, `src/autostart.rs`, toast on auto start.
Tests: config round-trip/defaults. Test command: `cargo test`.

## Task 6: Hardening
Device hot-swap via `IMMNotificationClient`, suspend/resume handling, release profile,
`build.rs` resources. 8-hour soak test is manual. Test command: `cargo test`.

## Verification
- `cargo test`: controller transitions, mixer drift/padding, Ogg round-trip, registry parsing.
- Manual: ffprobe 2ch 48 kHz; L voice / R system in Audacity; Teams test call auto start +
  toast, closes ~20 s after hangup; kill mid-recording → recovered; unplug headset →
  continues; Task Manager ≤ ~20 MB.

## Later (not v1)
Calendar naming, browser title sniffing, silence trim, retention, transcript folder watcher.
