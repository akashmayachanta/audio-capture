# Dictation: handoff to the next session

Written 2026-11-17. Read this first, then the spec and plan.

- Spec: `docs/superpowers/specs/2026-11-16-dictation-design.md`
- Plan: `docs/superpowers/plans/2026-11-16-dictation.md` (15 tasks)
- Ledger (scratch, delete when done): `docs/superpowers/plans/2026-11-16-dictation.progress.md`

**Project rules that apply:** never `git commit`/branch/push (everything stays uncommitted); tests first for logic; no tests for the Python transcriber; `cargo` from PowerShell, not Git Bash; `npm run check` must stay at 0 errors/warnings.

**User's last instruction:** stop; do not build or run tests; the user tests the app themself. Do not build or test unless they ask.

## What the feature is

Hold **Right Alt** to talk, release to transcribe and paste into the focused field. **Double-tap Right Alt** for hands-free (double-tap again to stop). Both keys are configurable (`[dictation]` in config.toml) and default to Right Alt. A glowing capsule overlay at the bottom of the screen shows listening / starting / transcribing / error. Audio is kept under `<output_dir>/dictation/YYYY/MM/`. The tray menu has "Start/Stop speech service". Dictation auto-starts the transcriber if it is down but never stops it.

## Status

| Task | State |
| --- | --- |
| 1 Config (`config::Dictation`, api.ts type) | done |
| 2 `keys.rs` | done |
| 3 `gesture.rs` | done |
| 4 `controller.rs` | done |
| 5 dictation folder excluded from meeting list/recovery (Rust + Python) | done |
| 6 `capture.rs` | done |
| 7 transcriber `/api/dictate`, `/api/health` | done, verified live |
| 8 `client.rs` | done |
| 9 `service.rs` + tray item + app wiring | done |
| 10 `paste.rs` | done, verified live |
| 11 `overlay_scene.rs` + `overlay.rs` | done, verified by screenshots |
| 12 `rawinput.rs` | done, verified live |
| 13 `Dictation` glue in `dictation/mod.rs` + `app.rs` | code written, compiles, 254 tests passed; **never run end to end** |
| 14 Settings "Dictation" card + Detect key button | code done, unit tests pass; **not seen in the real window** |
| 15 README/CLAUDE.md docs + final whole-branch review + manual checklist | docs done; **review and manual checklist not done** |

Last full run (2026-11-17, after Task 14): `cargo test --lib` 256 passed, 8 ignored. Frontend: `npm run check` 0 errors/0 warnings, `npm test` 51 passed. No release build was made.

## What remains

### Task 14: Settings UI and key detection (see the plan for the exact interfaces)
- `web_api/mod.rs`: `Host::detect_key(&self, timeout: Duration) -> Option<(String, String)>` and `POST /api/detect-key` → `{ "key": {"id": "RightAlt", "name": "Right Alt"} | null }` (test with the existing `FakeHost`).
- `window.rs`: `WindowHost::detect_key` via `dictation::rawinput::detect_next_key(5 s)`.
- `frontend/src/lib/keys.ts` (+ `keys.test.ts`): `COMMON_KEYS` (Right Alt first, then Right Ctrl, Left Alt, Caps Lock, Scroll Lock, Pause, Insert, Menu, F13–F24), `keyLabel(id)` (known → label, `vk:0x7B` → `Key 0x7B`, `""` → `Off`).
- `api.ts`: `local.detectKey()`.
- `Settings.svelte`: a "Dictation" `Card` (enable `Toggle`, "Hold to talk" and "Double-tap for hands-free" selects with a **Detect…** button each that waits ~5 s with "Press a key…", keep-audio toggle, hint that dictation needs the speech service). Look at the existing "Auto-record" and "Hotkeys" cards for the pattern.
- The user wants to bind Fn if the keyboard reports it; v1 only reads keyboard Raw Input, so Detect says "no key detected" for keyboards that hide Fn.

### Task 15
- Update `CLAUDE.md` (layout table rows for `src/dictation/*`, Config section `[dictation]`, transcriber routes `/api/dictate`, `/api/health`, the `MEETREC_TRANSCRIBER_ADDR` env var) and `README.md` (Dictation section: keys, requirements, audio folder, tray item).
- Run `.\build.ps1 -Test` and the manual checklist below.
- Whole-branch review by a fresh reviewer (the plan's Review Focus list), then fix Critical/Important findings test-first.
- Delete the ledger file when done.

## Manual checklist (nothing below has been done in the real tray app)
1. Rebuild, restart the tray (the user's tray runs the **installed** exe at `C:\Drive\Software\Apps\audio-capture\meetrec.exe`; use `copy-release.bat`).
2. Notepad: hold Right Alt, speak, release → text pasted; overlay appears ~150 ms after pressing, reacts to the voice.
3. After a hold in Notepad/Explorer, check the Alt **menu bar does not activate** (the code sends an unassigned key, VK 0xFF, when the hold is confirmed; untested). If it does, rework `Dictation::on_gesture` / `paste::send_menu_mask_key`.
4. Double-tap → listening; double-tap again → pasted. The first tap may flash a menu bar in some apps.
5. Right Alt + a letter (AltGr) types normally and does nothing else; a quick single tap does nothing; Esc cancels (while listening and while transcribing).
6. Speech service down → dictation starts it, overlay turns amber, then transcribes; `uv` missing → toast. Tray item toggles Start/Stop correctly.
7. Dictate during a meeting recording: both fine; dictation does not trigger auto-record (mic-usage detector excludes our own exe; not re-verified for the tray exe path).
8. A clipboard holding an image/files must survive a dictation (the text is typed, not pasted).
9. Dictation clips land in `<output_dir>/dictation/…` and do not appear in the Recordings list.
10. Cold mic: the first dictation after a long idle may lose ~0.5 s (the Intel SST mic once opened slowly).

## Files

New: `backend/recorder/src/dictation/{mod,keys,gesture,controller,capture,client,service,paste,overlay_scene,overlay,rawinput}.rs`, `backend/transcriber/app/dictation.py`.

Modified: `lib.rs`, `Cargo.toml` (windows features: DataExchange, Memory, KeyboardAndMouse, Gdi, UI_Input), `config.rs`, `app.rs`, `tray.rs`, `recordings.rs` (`DICTATION_DIR`, `is_dictation_root`), `session.rs` (`append_ext`/`rename_with_retry` now `pub(crate)`, recovery skips `dictation/`), `transcriber.rs` (`Launcher::kill_tree`), `frontend/src/lib/api.ts`, `frontend/src/lib/config.test.ts`, `backend/transcriber/app/{server,engines,jobs,audio,recordings}.py`.

## Rulings made (deviations from the plan; the user can overrule)
- **Overlay is a round microphone badge** (changed 2026-11-17 at the user's request; it was a capsule with bars). Not yet seen on screen in the tray, only as rendered frames.
- **Streaming transcription** (added 2026-11-17 at the user's request, not in the spec): phrases are cut at pauses in the capture thread (`segmenter.rs`) and sent one by one by `stream_worker`; `[dictation] paste_mode = "live" | "end"` (default `live`, the user's choice). `live` pastes per phrase in hands-free mode only; hold-to-talk pastes on release in both modes (Right Alt is still down, Ctrl+V would become Ctrl+Alt+V). Whole-clip fallback when no phrase was found. Verified with a synthetic-speech WAV against the live service (3 phrases, exact text); **not yet tried by voice in the tray**.
- **Overlay has no text:** messages ("No speech heard", errors) go to toasts; the overlay only changes colour/animation. This differs from the spec's "short messages in the overlay". Cost: no text on the overlay.
- **Non-text clipboard is typed over:** if the clipboard holds anything besides plain text (image, files, rich text) the text is typed with Unicode `SendInput` and the clipboard is untouched. Otherwise Ctrl+V, then the old text is restored after ~500 ms unless the clipboard changed meanwhile; history/cloud-exclusion formats are set. Elevated target windows → text left on the clipboard + toast.
- **Hand-written loopback HTTP client** (`client.rs`) instead of `ureq`; chunked replies unsupported (uvicorn JSON replies use Content-Length; verified live).
- **`MEETREC_TRANSCRIBER_ADDR`** env var overrides `127.0.0.1:8765` for scratch runs.
- **Transcriber:** engines take `turn=` (a context-manager factory) per piece/batch so a dictation cuts in between pieces; `/api/dictate` returns 500 if the model failed to load, 503 while loading, 409 if none chosen. Dictation uses `min_speech_secs=0.15`.
- **Controller extras:** `Hold+TapDiscarded` → discard + hide; `Hold+DoubleTap` → Toggle; late transcript results after Esc are ignored; max length ends listening and transcribes.
- **Gesture extras:** other keys already held block a press; key-ups missing for >10 s expire; auto-repeat ignored; the fake AltGr Ctrl (make code 0x21D) and VK 0xFF are ignored.
- **`RawKeys::start()`** returns `Receiver<Stamped>` (event + arrival `Instant`) to keep the 300 ms hold timing accurate.
- **`SpeechService`** (launcher + poller + stop plan) lives in `dictation/service.rs`; the tray item sits after the Capture submenu.
- **Paste runs on its own thread**; `Dictation::poll` returns toast messages to the app loop.
- **Overlay:** `render(scene, w, h, out)` scales with display DPI; `Scene.fade`; built-in `STATIC` window class.
- **Menu masking:** at the hold's `ShowOverlay`, if the hold key is Alt or Win, an unassigned key (VK 0xFF) is injected so releasing it does not open a menu/Start.

## Environment notes and gotchas
- **Build env on this machine:** portable MSVC + Ninja, no admin. Use `.\build.ps1` (from PowerShell) or, for quick cargo runs, set `CMAKE_GENERATOR=Ninja`, `CFLAGS=/FS`, `RC_PATH=<rc.exe on PATH>` first (cmake from `uv tool install cmake`). Plain `cargo test` without these fails in `audiopus_sys`. My helper `cg.ps1` was in the session scratchpad and is gone; recreate it if wanted.
- **Do not run a second `meetrec.exe` (even debug, even with `MEETREC_CONFIG`):** the user's installed tray holds the single-instance mutex and is usually running, and `App::new` runs `autostart::sync(config.start_with_windows)`, which **rewrites or deletes the user's real HKCU `Run\MeetRec` entry** whatever the scratch config says. The scratch config does not isolate the registry (a real hole in CLAUDE.md's advice). To test glue without the tray, drive `Dictation` from an `#[ignore]` test (it never touches the registry).
- **The user's transcriber may be running on 8765.** Never stop it. For live checks use a scratch server: a settings.toml with `port = 8799`, a scratch `recordings_dir`, a scratch `cache_dir` containing a copy of `%LOCALAPPDATA%\MeetRec\transcriber\models\whistle`, and env `MEETREC_TRANSCRIBER_SETTINGS=<that file>` + `MEETREC_TRANSCRIBER_ADDR=127.0.0.1:8799`. Test clips were made with Windows SAPI + PyAV (`libopus` encoder works in the transcriber venv).
- **Live tests overwrite the clipboard** and inject keys into the foreground window (`#[ignore]`d tests: `dictation::paste`, `dictation::rawinput`, `dictation::overlay::demo_every_phase`, `dictation::capture::records_a_second_from_the_mic`, `dictation::service::start_and_stop_the_real_service`). Save the clipboard first, run them only when the user agrees. Run ignored tests as `cargo test --lib <filter> -- --ignored --nocapture`.
- The paste live tests take the foreground with `AttachThreadInput`; they refuse to paste if the scratch edit box cannot get the foreground.

## Measured results (for reference)
- `/api/dictate`: a 6.6 s clip returned exact text in 0.7 s (Whistle, CPU); ~1.0 s while a 166 s meeting transcription was running (job still finished correctly).
- Overlay screenshots of all four phases looked right (teal / amber / indigo / red capsule with glow, above the taskbar).
