# Dictation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hold Right Alt (or double-tap it) to dictate into the focused field: capture the mic, show a listening overlay, transcribe through the local transcriber, paste the text. Plus a tray item to start/stop the speech service.

**Architecture:** Raw Input key events feed a pure gesture detector, which feeds a pure dictation controller (same style as `controller.rs`); the tray's `App` loop executes its actions: `CaptureStream` → mono Opus clip → `POST /api/dictate` on the transcriber → clipboard + Ctrl+V. The overlay is a plain Win32 layered window. Everything lives in a new `backend/recorder/src/dictation/` module; the transcriber gets `/api/dictate` and `/api/health`.

**Tech Stack:** Rust 2024 (`windows` 0.62 crate, `ringbuf`, `ogg`/libopus via `OggOpusWriter`, a small blocking HTTP client such as `ureq`), Python/FastAPI (transcriber), Svelte 5 + Vitest (Settings).

**Spec:** `docs/superpowers/specs/2026-11-16-dictation-design.md` (read it first; it holds the reasoning).

## Global Constraints

- **Never `git commit`/branch/push.** Every "commit" step is replaced by leaving changes uncommitted.
- Tests first for logic: `cargo test --lib` (from PowerShell, in `backend/recorder`); controller/gesture/state machines stay pure. No tests for the Python transcriber (verify by running it). Hardware/Win32-UI tests are `#[ignore]`.
- Frontend: pure logic in `frontend/src/lib/*.ts` with Vitest; `npm run check` stays at 0 errors/warnings.
- The tray process never loads tao/wry; the overlay is plain Win32.
- Defaults: `hold_key = "RightAlt"`, `toggle_key = "RightAlt"`, `enabled = true`, `keep_audio = true`, `max_secs = 300`. Timings: hold = down ≥ 300 ms; overlay shows after 150 ms held; double-tap = two taps with ≤ 400 ms between them; hold < 300 ms is a tap.
- Dictation audio: `<output_dir>/dictation/YYYY/MM/YYYY-MM-DD_HHMMSS.opus`, mono, `.opus.part` while writing, no sidecar.
- The transcriber is `127.0.0.1:8765`; dictation never stops it.
- Debug builds only while iterating (release LTO needs lots of RAM). Live runs use `AUDCAP_CONFIG` pointing at a scratch config.

## Review Focus

1. Right Alt used as AltGr/with another key (e.g. Right Alt+E) must cancel the gesture, never dictate (Task 3 tests).
2. Transcriber down, `uv` missing, or model still loading: audio kept, a clear message, nothing pasted, no hang (Tasks 4, 9, 13).
3. A silent or too-short clip: "No speech heard", nothing pasted (Tasks 4, 8).
4. A new gesture while already transcribing, or while a meeting recording is running, must not break either (Tasks 4, 13 manual).
5. Clipboard holding non-text (an image/file) must not be destroyed by the paste/restore (Task 12).

## File Structure

New, under `backend/recorder/src/dictation/`:

| File | Responsibility |
| --- | --- |
| `mod.rs` | `pub mod` list + `Dictation` (owns everything at runtime; Task 13) |
| `keys.rs` | `KeyId` parse/format/from-raw-input (pure) |
| `gesture.rs` | hold / tap / double-tap detector (pure) |
| `controller.rs` | dictation state machine (pure) |
| `capture.rs` | mic → level + mono Opus clip |
| `client.rs` | transcriber HTTP client |
| `service.rs` | speech-service state + tray label/stop decisions (pure) |
| `paste.rs` | clipboard + SendInput |
| `rawinput.rs` | Raw Input message-only window; `detect_next_key` |
| `overlay_scene.rs` | pure animation/pixel rendering |
| `overlay.rs` | Win32 layered window showing the scene |

Modified: `lib.rs`, `Cargo.toml`, `config.rs`, `app.rs`, `tray.rs`, `recordings.rs`, `session.rs`, `web_api/mod.rs`, `window.rs`, `backend/transcriber/app/{server,engines,jobs,audio,recordings}.py`, `frontend/src/lib/api.ts`, `frontend/src/routes/Settings.svelte`, new `frontend/src/lib/keys.ts`, `CLAUDE.md`, `README.md`.

---

### Task 1: Config

**Files:** Modify `backend/recorder/src/config.rs`, `frontend/src/lib/api.ts`.

**Interfaces:**
- Produces: `config::Dictation { enabled: bool, hold_key: String, toggle_key: String, keep_audio: bool, max_secs: u64 }` (`Default`, serde `#[serde(default)]`), field `Config.dictation: Dictation`; `Config::sanitized` clamps `max_secs` to 5..=600. TS `Config.dictation: { enabled; hold_key; toggle_key; keep_audio; max_secs }`.

- [ ] **Step 1: Tests** in `config.rs`: `dictation_defaults` (all defaults above); `old_config_without_dictation_section_parses` (`Config::parse("")` → defaults); `partial_dictation_section_keeps_other_defaults` (`"[dictation]\nkeep_audio = false"`); `max_secs_is_clamped` (0 → 5, 99999 → 600).
- [ ] **Step 2:** `cargo test --lib config::` fails (no such field).
- [ ] **Step 3:** Implement struct + default + sanitising. Key *validity* is checked where keys are parsed (Task 2), not here; an unparseable key is treated as "disabled" at runtime with a log warning.
- [ ] **Step 4:** Add the type to `api.ts`; `cargo test --lib config::` and `cd frontend; npm run check` pass.

### Task 2: Key ids

**Files:** Create `dictation/mod.rs` (empty `pub mod`s as tasks land), `dictation/keys.rs`; modify `lib.rs` (`pub mod dictation;`).

**Interfaces:**
- Produces: `#[derive(Clone, Debug, PartialEq, Eq, Hash)] pub struct KeyId(String)`; `KeyId::parse(&str) -> Option<KeyId>` (case-insensitive, trimmed; empty → `None`); `impl Display` (canonical name); `KeyId::from_raw(vkey: u16, make_code: u16, flags: u16) -> Option<KeyId>` (`flags` = `RAWKEYBOARD.Flags`, E0 bit = `0x02`); `pub fn display_name(&self) -> String`.
- Names: `LeftCtrl RightCtrl LeftAlt RightAlt LeftShift RightShift LeftWin RightWin CapsLock ScrollLock Pause Insert Menu F1..F24 Escape`; any other key `vk:0xNN`.

- [ ] **Step 1: Tests:** `parses_names_case_insensitively` (`"rightalt"` → `RightAlt`); `empty_and_unknown_do_not_parse` (`""`, `"banana"` → `None`); `from_raw` cases: `(0x12 VK_MENU, 0x38, E0)` → `RightAlt`, `(0x12, 0x38, 0)` → `LeftAlt`, `(0x11, 0x1D, E0)` → `RightCtrl`, `(0x10 VK_SHIFT, 0x36, 0)` → `RightShift` (shift has no E0; use make code 0x36 vs 0x2A), `(0x11, 0x21D, 0)` → `None` (the fake Ctrl Windows adds before AltGr), `(0x70, 0x3B, 0)` → `F1`; round trip `parse(&k.to_string()) == Some(k)` for each name.
- [ ] **Step 2:** run, fail. **Step 3:** implement. **Step 4:** pass.

### Task 3: Gesture detector

**Files:** Create `dictation/gesture.rs`.

**Interfaces:**
- Consumes: `KeyId`.
- Produces: `pub struct KeyEvent { pub key: KeyId, pub down: bool }`; `pub enum Gesture { Pressed, ShowOverlay, HoldReleased, TapDiscarded, DoubleTap, Cancelled }`; `pub struct Detector`; `Detector::new(hold: Option<KeyId>, toggle: Option<KeyId>) -> Self`; `fn key(&mut self, ev: &KeyEvent, now_ms: u64) -> Vec<Gesture>`; `fn tick(&mut self, now_ms: u64) -> Vec<Gesture>`; `fn set_keys(&mut self, hold: Option<KeyId>, toggle: Option<KeyId>)`.
- Semantics (same key for both): down → `Pressed`; held ≥150 ms (tick) → `ShowOverlay`; up ≥300 ms after down → `HoldReleased`; up <300 ms → `TapDiscarded`, except a tap whose down came ≤400 ms after the previous tap's up → `DoubleTap` (instead of `TapDiscarded`); any other key down while the key is down → `Cancelled` once, and its up emits nothing; auto-repeat downs are ignored. Different keys: the hold key never yields `DoubleTap`; the toggle key yields only `DoubleTap` (no `Pressed`/`TapDiscarded`); `None` disables that gesture.

- [ ] **Step 1: Tests** (times in ms): `quick_tap_is_pressed_then_discarded` (down@0, up@100 → `[Pressed]`, `[TapDiscarded]`); `hold_shows_overlay_then_releases` (down@0, tick@160 → `[ShowOverlay]` once even if ticked again, up@800 → `[HoldReleased]`); `double_tap_on_the_same_key` (tap 0–80, down@300, up@380 → last up yields `[DoubleTap]`); `slow_second_tap_is_a_new_tap` (gap 500 → `[TapDiscarded]`); `other_key_while_held_cancels` (RightAlt down, `E` down → `[Cancelled]`, RightAlt up → `[]`); `other_key_between_taps_breaks_the_double_tap` (tap, `E` down/up, tap → `[TapDiscarded]` on the last up); `auto_repeat_is_ignored`; `separate_toggle_key_only_double_taps` (hold=RightAlt, toggle=RightCtrl: RightCtrl tap → `[]`; two taps → `[DoubleTap]`, no `Pressed`); `no_hold_key_means_no_hold_gestures` (hold=None); `unrelated_keys_do_nothing_when_idle`.
- [ ] **Step 2:** run, fail. **Step 3:** implement as a small state struct driven only by the passed-in `now_ms` (no clock reads). **Step 4:** pass.

### Task 4: Dictation controller

**Files:** Create `dictation/controller.rs`.

**Interfaces:**
- Consumes: `Gesture`.
- Produces: `pub enum Input { Gesture(Gesture), Esc, TranscriptReady(String), TranscriptFailed(String), CaptureFailed(String), Tick { listening_secs: u64 }, Suspend }`; `pub enum Action { StartCapture, EnsureServer, ShowListening, HideOverlay, DiscardCapture, CancelCapture, FinishCapture, CancelTranscription, Paste(String), Notify(String) }`; `pub enum State { Idle, Pending, Hold, Toggle, Transcribing }` (`Pending` = key down, not yet known to be a hold); `Controller::new(max_secs: u64)`, `fn handle(&mut self, input: Input) -> Vec<Action>`, `fn state(&self) -> State`.
- Transitions to implement (these are the tests): `Idle+Pressed` → `[StartCapture]`, `Pending`; `Pending+ShowOverlay` → `[EnsureServer, ShowListening]`, `Hold`; `Pending+TapDiscarded` → `[DiscardCapture]`, `Idle`; `Pending|Hold+HoldReleased` → `[FinishCapture]`, `Transcribing`; `Pending+DoubleTap` → `[EnsureServer, ShowListening]`, `Toggle`; `Idle+DoubleTap` (separate keys) → `[StartCapture, EnsureServer, ShowListening]`, `Toggle`; `Pending|Hold+Cancelled` → `[CancelCapture, HideOverlay]`, `Idle`; `Toggle+DoubleTap|HoldReleased` → `[FinishCapture]`, `Transcribing`; `Toggle+Pressed|TapDiscarded|ShowOverlay` → `[]`; `Esc` in Pending/Hold/Toggle → `[CancelCapture, HideOverlay]`, in Transcribing → `[CancelTranscription, HideOverlay]`, in Idle → `[]`; `Transcribing+any Gesture` → `[]`; `TranscriptReady("x")` → `[Paste("x"), HideOverlay]`, `Idle`; `TranscriptReady("  ")` → `[HideOverlay, Notify("No speech heard")]`; `TranscriptFailed(m)`/`CaptureFailed(m)` → `[HideOverlay, Notify(m)]`, `Idle`; `Tick` with `listening_secs >= max_secs` in Hold/Toggle → `[FinishCapture]`; `Suspend` → cancel like `Esc`.

- [ ] **Step 1:** one test per transition above (name them after the transition, e.g. `tap_is_discarded`, `double_tap_toggles_on_and_off`, `second_trigger_while_transcribing_is_ignored`, `empty_transcript_pastes_nothing`). **Step 2:** fail. **Step 3:** implement (a `match (state, input)`). **Step 4:** pass.

### Task 5: Keep dictation clips out of the meeting list

**Files:** Modify `recordings.rs` (`find_opus`), `session.rs` (`find_parts`); `backend/transcriber/app/recordings.py` (`list_recordings`).

- [ ] **Step 1: Tests** in `recordings.rs`: `dictation_folder_is_not_listed` (a `dictation/2026/11/x.opus` under the root is skipped; `2026/11/y.opus` is listed); in `session.rs`: `recover_parts_ignores_the_dictation_folder` (a `dictation/…/x.opus.part` is left untouched).
- [ ] **Step 2:** fail. **Step 3:** skip a directory named `dictation` directly under `output_dir` (pass the root down, compare at depth 1). Python: filter paths whose first part relative to `root` is `dictation`. **Step 4:** `cargo test --lib recordings:: session::` pass.

### Task 6: Dictation capture

**Files:** Create `dictation/capture.rs`; modify `Cargo.toml` if a feature is needed.

**Interfaces:**
- Consumes: `CaptureStream::spawn(Source::Mic, Option<String>, HeapProd<f32>)`, `OggOpusWriter::new(w, 1, bitrate_bps, &tags)` / `write_frame(&[f32; FRAME])` / `finish()`, `audio::FRAME` (960), `SAMPLE_RATE`.
- Produces: `pub struct Clip { pub path: PathBuf, pub bytes: Vec<u8>, pub secs: f64 }`; `pub fn clip_path(dir: &Path, start: DateTime<Local>) -> PathBuf` (the `dictation/YYYY/MM/…_HHMMSS.opus` form, `_2` suffix if taken); `pub struct Capture`; `Capture::start(cfg: &Config, now: DateTime<Local>) -> Result<Capture>`; `fn level(&self) -> f32` (0..1 RMS of the last frames, thread-safe); `fn elapsed_secs(&self) -> f64`; `fn finish(self, keep_audio: bool) -> Result<Option<Clip>>` (`None` when shorter than 0.3 s of audio); `fn cancel(self, keep_audio: bool)`; `fn is_alive(&self) -> bool`.
- Internals: a thread drains the ring buffer in 960-sample frames into the writer and publishes the level; `pump(cons, writer, level) -> usize` is a separate function so it can be tested with a `HeapRb`.

- [ ] **Step 1: Tests:** `clip_path_has_date_folders_and_a_suffix_when_taken`; `pump_encodes_whole_frames_only` (push 2.5 frames of samples → returns 2, 480 samples remain); `level_tracks_amplitude` (silence → ~0, 0.5-amplitude sine → > 0.2, clamped ≤ 1); `short_clip_is_none` (0.1 s pumped then `finish` → `None`, no file left); `finish_renames_part_and_returns_bytes` (1 s of sine → `.opus` exists, no `.part`, `bytes` starts with `OggS`, `secs ≈ 1.0`); `keep_audio_false_deletes_the_file_but_returns_bytes`; an `#[ignore]` hardware test `records_a_second_from_the_mic`.
- [ ] **Step 2:** fail. **Step 3:** implement; the writer goes to `<name>.opus.part`, renamed on finish (reuse the retry rename helper from `session.rs` if it is `pub(crate)`, else expose it). **Step 4:** pass.

### Task 7: Transcriber API (Python)

**Files:** Modify `server.py`, `engines.py`, `jobs.py`, `audio.py`. No unit tests (project rule); verify by running.

**Interfaces:**
- Produces: `GET /api/health` → `{"ok": true}`; `POST /api/dictate` (body = Ogg Opus bytes, ≤ 20 MiB) → `{"text": str, "model": str, "secs": float}`; 409 `no model selected`; 503 `model loading` (while `engines.status()["loading"]`); 400 undecodable audio. `audio.decode_bytes(data: bytes) -> np.ndarray` (16 kHz mono float32; first channel only, via `av.open(io.BytesIO(data))`). `EngineCache.dictation()` context manager and `EngineCache.pause_point()`; both engines' `transcribe(..., pause_point=lambda: None)` call it before each piece/batch.
- Behaviour: dictation gets priority at piece boundaries: while a `dictation()` block is active `pause_point()` blocks batch jobs; the block waits for the piece in flight to finish before using the engine. `Jobs.transcribe_recording` passes `engines.pause_point`. Dictation reuses `chunking.plan` + the active engine and joins piece texts with a space. CORS needs no change (the tray is not a browser).

- [ ] **Step 1:** implement. **Step 2: Verify** (PowerShell, `backend/transcriber`, scratch settings via `AUDCAP_TRANSCRIBER_SETTINGS`): start the server, `curl.exe -s http://127.0.0.1:8765/api/health` → `{"ok":true}`; POST a real recording clip: `curl.exe -s -X POST --data-binary "@<file>.opus" http://127.0.0.1:8765/api/dictate` returns text once Whistle is loaded and 503/409 before; start a meeting transcription and POST a dictation during it → the dictation returns within about one piece, and the job still completes.

### Task 8: Transcriber client (Rust)

**Files:** Create `dictation/client.rs`; modify `Cargo.toml` (HTTP client, no TLS).

**Interfaces:**
- Produces: `pub enum DictateError { Unreachable, Loading, NoModel, Failed(String), Cancelled }`; `pub struct Client` (`Client::new(base: &str)`, default `http://127.0.0.1:8765`); `fn health(&self) -> bool` (2 s timeout); `fn dictate(&self, opus: &[u8], cancel: &AtomicBool) -> Result<String, DictateError>` (timeout 10 min, polls `cancel`); `fn shutdown(&self) -> bool`.

- [ ] **Step 1: Tests** with a stub `std::net::TcpListener` server: `dictate_returns_the_text` (200 `{"text":"hello"}` → `"hello"`); `503_is_loading`; `409_is_no_model`; `500_is_failed_with_the_detail`; `closed_port_is_unreachable`; `health_true_on_200_false_otherwise`; `cancel_flag_aborts` (stub never replies; set the flag after 100 ms → `Cancelled` within 1 s).
- [ ] **Step 2:** fail. **Step 3:** implement (run the request on a worker thread if the library can't be interrupted, so cancel works). **Step 4:** pass.

### Task 9: Speech-service state and tray item

**Files:** Create `dictation/service.rs`; modify `tray.rs`, `app.rs`.

**Interfaces:**
- Produces: `pub enum ServiceState { Down, Starting, Up, Stopping }`; `ServiceState::derive(api_up: bool, child_running: bool, stopping: bool) -> Self`; `pub fn menu_item(state: ServiceState) -> (&'static str, bool)` (text, enabled): Down → `("Start speech service", true)`, Starting → `("Starting speech service…", false)`, Up → `("Stop speech service", true)`, Stopping → `("Stopping speech service…", false)`; `pub enum StopPlan { Shutdown, KillChild, CannotStop }`; `pub fn stop_plan(api_up: bool, have_child: bool) -> StopPlan` (API up → `Shutdown`; else child → `KillChild`; else `CannotStop`). `MenuCommand::SpeechService`; `Tray::set_service(state)`.

- [ ] **Step 1: Tests:** `derive` table (api up → `Up`; api down + child running → `Starting`; both down → `Down`; stopping + api up → `Stopping`; stopping + api down → `Down`); `menu_item` texts; `stop_plan` table.
- [ ] **Step 2:** fail. **Step 3:** implement; `Tray::new` adds the item after "Add marker" with a separator (default text "Start speech service"); `command_for` maps it. In `app.rs`: own a `transcriber::Launcher` and a `Client`; a thread polls `client.health()` every 3 s into an `mpsc` channel; on `SpeechService`: Down → `find_dir(&config.transcriber_dir, &exe)` + `Launcher::start` (missing dir/`uv` → `notify`), Up → per `stop_plan` (`client.shutdown()` / kill the launcher's child tree / `notify("Couldn't stop the speech service")`). Update the tray item every loop pass from `derive(...)`. **Step 4:** tests pass; manual: right-click shows the item; Start → "Starting…" → "Stop speech service"; Stop → back to Start.

### Task 10: Paste

**Files:** Create `dictation/paste.rs`; add `windows` features (`Win32_System_DataExchange`, `Win32_System_Memory`, `Win32_UI_Input_KeyboardAndMouse`).

**Interfaces:**
- Produces: `pub enum PasteOutcome { Pasted, LeftOnClipboard }`; `pub fn paste_text(text: &str) -> Result<PasteOutcome>`; pure `pub fn restore_plan(previous: Previous) -> Restore` with `Previous { Text(String), Empty, NonText }` and `Restore { Text(String), Clear, Leave }` (`Text` → restore it; `Empty` → clear; `NonText` → `Leave`, i.e. never wipe an image/file the user had copied).
- Behaviour: wait (≤ 1.5 s) until Right/Left Alt, Ctrl, Shift, Win are physically up (`GetAsyncKeyState`); read the previous clipboard; set the text with the history/cloud-exclusion formats (`CanIncludeInClipboardHistory = 0`, `CanUploadToCloudClipboard = 0`, `ExcludeClipboardContentFromMonitorProcessing`); `SendInput` Ctrl+V; restore after 500 ms per `restore_plan`; if the foreground window is elevated and `SendInput` reports 0 events sent → `LeftOnClipboard`.

- [ ] **Step 1: Tests:** `restore_plan_table` (the three cases above); `#[ignore]` `paste_roundtrip_in_a_scratch_edit_box` (create a Win32 EDIT control, focus it, paste "héllo", read it back).
- [ ] **Step 2:** fail. **Step 3:** implement. **Step 4:** pass; run the ignored test once manually.

### Task 11: Overlay

**Files:** Create `dictation/overlay_scene.rs`, `dictation/overlay.rs`; add `windows` feature `Win32_Graphics_Gdi`.

**Interfaces:**
- `overlay_scene`: `pub const W: usize = 320; pub const H: usize = 96;` `pub enum Phase { Listening, Starting, Transcribing, Error }`; `pub struct Scene { pub phase: Phase, pub level: f32, pub t: f32, pub dark: bool }`; `pub fn render(scene: &Scene, out: &mut [u32])` (premultiplied BGRA, `W*H` pixels); `pub struct Smoother`; `Smoother::push(&mut self, level: f32, dt: f32) -> f32` (fast attack, slow release).
- Look: a glowing capsule with ~5 bars/orb that swells with `level` (quiet = gentle breathing), `Starting` = amber pulse, `Transcribing` = travelling shimmer, `Error` = red flash; accent colour from the app's tokens.
- `overlay`: `pub struct Overlay`; `Overlay::new() -> Result<Overlay>` (hidden); `fn show(&mut self, phase: Phase)`; `fn set_level(&mut self, level: f32)`; `fn hide(&mut self)`; `fn tick(&mut self)` (redraw at ~60 fps while visible, called from the `App` loop; `IDLE_WAIT_MS` drops to 16 ms while shown). Window: `WS_EX_LAYERED | TOPMOST | TOOLWINDOW | NOACTIVATE | TRANSPARENT`, bottom-centre of the monitor under the cursor, `UpdateLayeredWindow` with per-pixel alpha, never activated.
- Messages ("No speech heard", errors) go to toasts via `App::notify`, not into the overlay (no text rendering; **deviation from the spec's "short messages in the overlay", flag to the user**).

- [ ] **Step 1: Tests (scene only):** `corners_are_transparent`; `centre_is_visible`; `louder_level_lights_more_pixels` (sum of alpha at level 1.0 > at 0.0, Listening); `transcribing_frames_differ_over_time`; `error_phase_is_reddish` (R channel > B channel at the centre); `smoother_attacks_fast_and_releases_slowly` (push 1.0 once → ≥ 0.6; then push 0.0 once → still ≥ 0.3).
- [ ] **Step 2:** fail. **Step 3:** implement `overlay_scene` (software drawing, no deps, like `icon.rs`), then `overlay.rs`. **Step 4:** scene tests pass. **Step 5 (manual):** a throwaway `--overlay-demo` is not added; verify in Task 13.

### Task 12: Raw Input and key detection

**Files:** Create `dictation/rawinput.rs`; add `windows` features (`Win32_UI_Input`, `Win32_Graphics_Gdi` if needed for message windows).

**Interfaces:**
- Consumes: `KeyId::from_raw`, `KeyEvent`.
- Produces: `pub struct RawKeys` with `RawKeys::start() -> Result<(RawKeys, Receiver<KeyEvent>)>` (its own thread with a message-only window, `RegisterRawInputDevices` for keyboards with `RIDEV_INPUTSINK`; emits an event for every key down/up, including keys it can't name as `vk:0xNN`; ignores the fake AltGr Ctrl via `KeyId::from_raw` returning `None`; also emits Esc); `Drop` stops the thread. `pub fn detect_next_key(timeout: Duration) -> Option<(KeyId, String)>` (own temporary window; returns the first key *down*, with `KeyId::display_name`).

- [ ] **Step 1:** pure mapping is already tested in Task 2; add `#[ignore]` `detects_a_synthetic_keypress` (inject a key with `SendInput`, expect an event) as the live test. **Step 2:** implement. **Step 3:** run the ignored test manually.

### Task 13: Wire it into the tray app

**Files:** Modify `dictation/mod.rs`, `app.rs`.

**Interfaces:**
- Consumes: everything above. Produces: `pub struct Dictation` with `Dictation::new(cfg: &Config) -> Result<Dictation>`, `fn apply_config(&mut self, cfg: &Config)`, `fn poll(&mut self, now: Instant) -> Vec<Notice>` (drains key events → `Detector` → `Controller`, executes actions, returns toast texts), `fn wants_fast_loop(&self) -> bool`.
- Behaviour: `StartCapture` → `Capture::start` (failure → `CaptureFailed`); `ShowListening` → overlay `Listening`, or `Starting` while the server is down; `EnsureServer` → start via the same path as the tray item (Task 9) if `client.health()` fails; `FinishCapture` → `capture.finish(keep_audio)`, `None` → "No speech heard"; else overlay `Transcribing`, on a worker thread: wait up to 90 s for the API/model (`Unreachable`/`Loading` retries every 1 s; give up with the message "The speech service did not start"), then `client.dictate`, send `TranscriptReady`/`TranscriptFailed` back; `Paste(text)` → `paste_text` (`LeftOnClipboard` → toast); `CancelTranscription` sets the cancel flag; `CancelCapture`/`DiscardCapture` → `cancel(keep_audio)`; `Tick` each second with the capture's elapsed seconds; `Suspend` on power events. `Esc` events are fed only to the controller. Config hot-reload re-applies keys (`KeyId::parse`; bad value → disabled + warning log). A meeting recording is unaffected; nothing here touches `Controller`/`Recorder`.
- `Dictation::new` failing (Raw Input) → log + toast once; the rest of `App` starts normally.

- [ ] **Step 1:** add a testable seam: `Dictation::step(&mut self, input: Input) -> Vec<Action>` is just the controller; unit tests for `apply_config` key parsing (`""` and `"nope"` → that gesture disabled, `"rightalt"` → enabled). **Step 2:** implement and call `dictation.poll` from the `App::run` loop; shorten the wait to 16 ms while the overlay is visible.
- [ ] **Step 3: Manual verification (scratch config via `AUDCAP_CONFIG`, debug build, transcriber started from the tray item):** hold Right Alt in Notepad, say a sentence, release → text pasted; the overlay shows within ~150 ms and reacts to the voice; double-tap → listening, double-tap → pasted; Right Alt+E types normally and does nothing else; quick Right Alt tap does nothing; Esc cancels; stop the server and dictate → toast, clip kept in `dictation/`; start a meeting recording and dictate during it → both fine; Alt-menu check (Notepad/Explorer) after a hold: no menu bar activation (if it activates, add the unassigned-key trick from the spec in `rawinput.rs`/`paste.rs`); confirm the auto-record detector does not trigger on dictation (the mic-usage exclusion covers the tray exe; if not, fix there).

### Task 14: Settings UI and key detection

**Files:** Modify `web_api/mod.rs`, `window.rs`, `frontend/src/routes/Settings.svelte`; create `frontend/src/lib/keys.ts` (+ `keys.test.ts`).

**Interfaces:**
- Produces: `Host::detect_key(&self, timeout: Duration) -> Option<(String, String)>` (id, display name); `POST /api/detect-key` → `{ "key": {"id": "RightAlt", "name": "Right Alt"} | null }`; `WindowHost` implements it via `rawinput::detect_next_key(5 s)`. TS: `COMMON_KEYS: {id: string; label: string}[]` (Right Alt first, then Right Ctrl, Left Alt, Caps Lock, Scroll Lock, Pause, Insert, Menu, F13–F24), `keyLabel(id: string): string` (known → label, `vk:0x7B` → `Key 0x7B`, `""` → `Off`), `local.detectKey()` in `api.ts`.
- UI: a "Dictation" card (enable `Toggle`, two key selects "Hold to talk" / "Double-tap for hands-free" — each lists `COMMON_KEYS` plus "Off" plus the current value if custom — each with a **Detect…** button that disables the form for 5 s with "Press a key…" and fills the select; a "Keep audio" toggle; a hint "Dictation needs the speech service running; double-tap starts it").

- [ ] **Step 1: Tests:** `web_api` `detect_key_returns_the_key_or_null` (`FakeHost` returning `Some`/`None`); `keys.test.ts`: `keyLabel` cases above, `COMMON_KEYS` has unique ids and starts with `RightAlt`. **Step 2:** fail. **Step 3:** implement. **Step 4:** `cargo test --lib web_api::`, `cd frontend; npm test; npm run check` pass. **Step 5 (manual, CDP per CLAUDE.md):** Settings shows the card, Detect picks a pressed key, Save writes `[dictation]` to the scratch config and the running tray picks it up without restart.

### Task 15: Docs and final verification

**Files:** Modify `CLAUDE.md` (layout table rows for the new files, Config section, Run modes unchanged, Transcription API routes), `README.md` (a "Dictation" section: keys, requirements, audio folder, tray item).

- [ ] **Step 1:** update docs. **Step 2:** `.\build.ps1 -Test` (Vitest + `cargo test --lib`) passes; `cd frontend; npm run check` 0 errors/warnings. **Step 3:** `cargo test --lib -- --ignored` for the hardware/Win32 tests that apply on this machine; report any that cannot run. **Step 4:** re-run the Task 13 manual checklist against the final build and report each line. Leave everything uncommitted.
