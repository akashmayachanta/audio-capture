# Dictation: speech-to-text into any text field

Status: draft for approval. Not committed (project rule: the user commits).

## Intent and why

AudCap already records meetings and can transcribe them afterwards with a local
model. The user also wants to *dictate*: press a shortcut, talk, and have the
words appear in whatever text field currently has focus, in any app. While
listening, a small animation (like a phone's smart assistant) shows that the
mic is live and reacting to the voice.

Two ways to start, both the user's:

- **Hold to talk.** Hold the dictation key; speak; release to transcribe and paste.
- **Double-tap to toggle (hands-free).** Double-tap a chosen key to start
  listening (and start the transcriber server if it is not running); double-tap
  again to stop, transcribe and paste. The user first asked for the Fn key;
  Windows usually cannot see Fn (firmware-handled), so the key is configurable
  and Settings has a **Detect…** button: if the keyboard reports Fn it can be
  bound.

**Decided by the user: both default to the Right Alt key** (one key: hold it to
talk, double-tap it for hands-free). Both are separately configurable.

## Goals

- Hold-to-talk and double-tap toggle both work from any foreground app.
- Text lands in the focused field within a few seconds of stopping
  (Whistle: about 1–4 s for a 10 s utterance).
- A listening animation that appears instantly, never steals focus, and
  reacts to the mic level.
- Dictation audio is kept (user choice) in a **separate subfolder** of
  `output_dir`, `dictation/`, and does not show up in the meeting list.
- Works while a meeting recording is running (two shared-mode mic clients).
- Reuses the transcriber and the model already loaded; no second model stack.

## Non-goals

- No streaming partial text while speaking (v1 transcribes on release; chunked
  transcription of finished pauses is a later optimisation).
- No stopping the transcriber server when dictation ends. The server stays up
  (reloading Bark costs 1–2 min).
- No new models, no cloud services, no per-app behaviour, no commands
  ("new line", punctuation words) in v1.
- Not a replacement for meeting recording; dictation sessions never use the
  recorder's controller/auto rules.

## Anti-goals (would make it a failure even if it "works")

- Stealing focus from the field being dictated into.
- Pasting the wrong chord (target app seeing Ctrl+Alt+V instead of Ctrl+V).
- Silently doing nothing when the transcriber is down or no model is loaded.
- Dictation triggering meeting auto-record (the mic-usage detector already
  excludes our own exe; keep it that way).
- Slowing normal typing (no low-level keyboard hook).

## Constraints from the existing code

- The tray process never loads the GUI stack (tao/wry). The overlay must be
  plain Win32.
- `hotkeys.rs` uses `global_hotkey` 0.9. On Windows it registers with
  `RegisterHotKey` (`MOD_NOREPEAT`) and, after a press, polls
  `GetAsyncKeyState` every 50 ms to emit a `Released` event, so hold-to-talk
  is supported, with up to ~50 ms release latency. It reports the release of
  the **main key only**; the modifiers may still be down.
- `RegisterHotKey` cannot bind a lone modifier, so the double-tap needs a
  different mechanism (Raw Input, below).
- The transcriber (`backend/transcriber`) is batch-only: file → chunks →
  `.md`. One worker thread, one loaded model, and the engines are not
  thread-safe (a Whistle instance holds one native model). A meeting
  transcription can run for a long time, so dictation must be able to cut in.
- The tray has no HTTP client yet (the window uses `fetch`). The transcriber
  listens on `127.0.0.1:8765`; its Host-header check and CORS (`audcap.localhost`
  only) stay as they are.
- `Launcher::start` (`src/transcriber.rs`) starts the server hidden via `uv`,
  and a second start while it runs is a no-op. The window process owns the
  existing launcher; the tray needs its own use of the same code.
- Project rules: tests first for logic (`cargo test --lib`), controller/state
  machines pure; no tests for the Python transcriber; `npm run check` stays at
  0 errors; no git commits.

## Approaches considered

**Where audio is captured and transcribed**

1. *Tray captures the mic, transcriber server transcribes.* **Chosen.**
   Reuses WASAPI capture and the loaded model; works beside a recording.
2. Window process (WebView2) does everything. Rejected: cold start too slow
   for press-and-talk.
3. Transcriber captures the mic itself. Rejected: duplicates device selection,
   adds a dependency, and the server is optional.

**Overlay**

1. *Native Win32 layered window in the tray process.* **Chosen.** Instant,
   no-activate, no WebView. Cost: hand-written drawing code.
2. Transparent WebView2 window with CSS animation. Rejected: start-up lag and
   focus-steal risk.

**Putting the text in the field**

1. *Clipboard + simulated Ctrl+V, then restore the previous clipboard text.*
   **Chosen.** Works in terminals, browsers, Electron.
2. `SendInput` Unicode typing. Rejected: slow for long text, flaky in some apps.

**Detecting the double-tap**

1. *Raw Input (`RegisterRawInputDevices`, `RIDEV_INPUTSINK`).* **Chosen.**
   Passive, cannot delay or swallow typing, and may expose Fn on keyboards that
   report it. It also powers the Detect button.
2. `WH_KEYBOARD_LL` hook. Rejected: sits in the typing path of every key press.
3. A second chord instead of a double-tap. Offered, not chosen.

## Design

### Flow

```
hold key ───┐                                   ┌─ overlay: listening orb (level-driven)
            ├─▶ dictation controller ─▶ mic capture ─▶ .opus (dictation/)
double-tap ─┘   (pure state machine)        │
(Raw Input key events → pure gesture detector)
                                            ▼ on stop
                          POST audio bytes → transcriber /api/dictate → { text }
                                            ▼
                     overlay "Transcribing…" → clipboard + Ctrl+V → restore clipboard
```

Both triggers feed one pure state machine (`dictation` controller, same style
as `controller.rs`): `Idle → Listening(hold | toggle) → Transcribing → Idle`.
Inputs: hold pressed/released, double-tap, transcript ready/failed, cancel (Esc).
Actions: start/stop capture, show/hide overlay, request transcription, paste,
start the transcriber. A hold that is shorter than a minimum (for example
300 ms) is discarded as an accidental press. A new trigger while
`Transcribing` is ignored. Esc while listening **or transcribing** cancels
without pasting (the audio is kept; the server may finish the piece it is on).

### Triggers

- **Hold key and double-tap key.** Both are lone keys (default **Right Alt**
  for each), so neither can use `RegisterHotKey`; both come from Raw Input via
  one pure gesture detector in `rawkeys`/`dictation`. The detector classifies a
  key's press by duration:
  - *Hold*: key down held longer than ~300 ms with no other key down. Listening
    starts at the press (capture runs, the overlay appears after ~150 ms so a
    quick tap does not flash it) and the release stops, transcribes and pastes.
  - *Tap*: down+up within ~300 ms with no other key between; the capture
    started by that press is discarded silently.
  - *Double-tap*: two taps within ~400 ms; the second tap's release starts
    toggle-mode listening (the first word may be clipped by the tap, accepted).
    Pressing and holding the second press instead is simply a hold.
  - When the hold key and the double-tap key differ, each key runs its own
    detector (hold only for the hold key, tap-pair only for the double-tap key).
  - A tap requires that no other key goes down between its down and up, so
    Right Alt+key shortcuts are neither holds nor taps. If another key goes down
    while the key is held, the dictation is cancelled (it was a shortcut).
  - **Right Alt specifics.** On layouts where Right Alt is AltGr, Windows adds a
    fake Left Ctrl event (scan code 0x21D / `VK_LCONTROL` immediately before
    `VK_RMENU`): the detector must ignore that synthetic Ctrl. Because Raw Input
    cannot swallow keys, a lone Alt press/release can activate an app's menu
    bar; after a hold or tap ends, inject a harmless unassigned key
    (`VK_0xFF` down/up) before the Alt release is seen by the app where
    possible, or at minimum right after, to cancel menu activation. The builder
    verifies this in Notepad and Explorer.
  - Chosen key is stored as a key id
  (virtual-key/scan code **with side**: left and right Ctrl/Alt/Shift/Win are
  different keys). Default: `RightAlt` for both (user's decision); an empty
  value disables that gesture. **v1 reads keyboard Raw Input only** (`RAWKEYBOARD`):
  Fn is bindable only if the keyboard reports it that way; vendor HID usages
  (`RIM_TYPEHID`, id form `hid:<page>:<usage>`) are a follow-up unless the
  builder finds them cheap. Detect says "no key detected" when nothing arrives.
  While Listening by hold, taps are ignored; while
  Listening by toggle, the next double-tap (or a hold press) stops it.
- **Settings → Dictation card:** enable switch, hold key picker and double-tap
  key picker (each: a list of common keys with Right Alt as the default, plus
  a **Detect…** button), keep-audio switch, and a note when the transcriber or model is
  not ready. Detect is a window-process call (`POST /api/detect-key` on the
  `audcap://` API, host-implemented): it opens a message-only Raw Input
  window, waits up to ~5 s for the next key, and returns its id and display
  name, or "no key detected".

### Capture

Reuse `audio::capture::CaptureStream` (mic source, configured `mic_device`,
48 kHz mono f32) feeding a small encoder to a mono Ogg Opus file via the
existing `encode::opus_ogg` (mapping family 0). Files are written to
`<output_dir>/dictation/YYYY/MM/YYYY-MM-DD_HHMMSS.opus` (`.part` while
writing, same rename-on-close lifecycle as `session.rs`, no sidecar). If
keep-audio is off the file is deleted after a successful paste. A maximum
length (say 5 min) stops listening automatically. The recorder's listing
(`recordings.rs`) must not descend into `dictation/`.

The mic level (RMS over ~20 ms frames) is published to the overlay.

### Overlay

A Win32 layered, topmost, tool-window, `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT`
window (click-through, never focused), per-pixel alpha via `UpdateLayeredWindow`,
bottom-centre of the monitor under the cursor, driven by a ~60 fps timer on the
tray thread while visible. Look: a soft glowing capsule with a few bars or a
gradient orb that swells with the mic level (idle breathing when quiet), a
different state while Transcribing (travelling shimmer), and short messages
("Starting transcriber…", "Loading model…", "No speech heard", error text) that
fade out. Colours follow the app's tokens (accent = "me" colour); light/dark
follows the system theme. The drawing is software-rendered and dependency-free,
like `icon.rs`; the shape/animation maths is pure and unit-tested.

### Transcription (transcriber side)

- New `POST /api/dictate`: body is the Ogg Opus bytes (mono), response
  `{ "text": "…", "model": "…", "secs": … }`. No path is passed, so it does not
  matter whether `output_dir` and the transcriber's recordings folder match.
  A size cap and the existing Host check apply. 409 if no model is selected,
  503 while the model is still loading (the tray shows "Loading model…" and
  retries).
- It reuses `audio.decode` (from bytes), `chunking.plan` and the active engine,
  joins the piece texts with spaces, and returns the text. Single-channel
  input (no channel map).
- **Priority over meeting jobs.** A running batch job must yield to dictation at
  piece boundaries: a small gate (or lock taken per piece) in the engines so
  `/api/dictate` runs between two pieces of a long job and the job then
  continues. Mechanism is the builder's choice; dictation must not wait for a
  whole meeting transcription.
- Python side is not unit-tested (project rule); the Rust side tests the client
  against a stub.

### Starting the server

When a trigger fires and the API on `127.0.0.1:8765` does not answer, the tray
starts it (reusing the `find_dir` + `Launcher::start` logic, with the tray
owning a launcher), keeps listening/recording in the meantime, and on stop waits
(overlay: "Starting transcriber…" then "Loading model…") up to a limit for the
API and model, then transcribes. If it never comes up, or `uv`/the transcriber
dir is missing, the overlay and a toast say why, and the audio is kept.
The server is never stopped by dictation.

### Tray menu: start/stop the speech service

The tray's right-click menu gets one item that starts or stops the speech
(transcriber) service, next to the recording items and in the same style as
"Start recording" / "Stop recording" (`MenuItem::set_text`):

- Service down → **Start speech service**.
- Service up → **Stop speech service**.
- While starting or stopping → the item is disabled and reads "Starting speech
  service…" / "Stopping speech service…". (Model loading after the API is up is
  shown by the window/overlay, not here.)

Behaviour: Start uses the same `find_dir` + `Launcher::start` path as the
dictation auto-start; a missing `uv` or transcriber folder shows a toast with the
reason. Stop sends `POST /api/shutdown`, the same call as the window's Stop
button; if the API does not answer but this tray started a process, it falls back
to ending that process tree (as `copy-release.bat` does). The tray learns the
state by a cheap periodic check of the API on `127.0.0.1:8765` (a tiny
`GET /api/health`, or a plain `/api/state` call if the builder prefers not to add
a route), so it also reflects a server started by the window or by hand.
This is separate from dictation: a dictation never stops the server, but the user
can stop it here, and the next dictation starts it again.

### Pasting

On a transcript: wait until the dictation key and any modifiers are
physically up (poll `GetAsyncKeyState`, with a timeout), save the current
clipboard text, set the clipboard to the dictated text, send Ctrl+V with
`SendInput`, then restore the previous text after a short delay. Only text
clipboard contents are restored; other formats on the clipboard are lost
(documented limitation). If the foreground window is elevated (UIPI blocks
`SendInput`), leave the text on the clipboard and say so in a toast. If the
text is empty, show "No speech heard" and paste nothing.

### Config

`config.toml` gains a `[dictation]` section (all with defaults, sanitised like
the rest). No new `[hotkeys]` entry: the keys are Raw Input key ids.

```
[dictation] enabled = true, hold_key = "RightAlt", toggle_key = "RightAlt",
            keep_audio = true, max_secs = 300
```

The audio folder is derived (`<output_dir>/dictation`), not configurable in v1.
Config hot-reload re-applies the Raw Input keys like it does for hotkeys today.

### Errors and edge cases

- No mic / capture failure: overlay error, nothing pasted.
- Transcriber unreachable or errored: audio kept, text not lost silently: the
  overlay and a toast say what happened; the saved `.opus` can be
  transcribed later from its folder.
- A meeting recording starting or stopping mid-dictation does not affect either.
- Sleep/resume: any dictation in progress is cancelled, as for recordings.
- Raw Input registration failure: logged and shown as a toast; the rest of the
  app is unaffected.

## Left to the builder

- Tap/hold timings (the numbers above are starting points), overlay visuals and
  exact animation, and the exact menu-bar-suppression trick for Alt.
- HTTP client choice for the tray (small blocking client such as `ureq`).
- How the tray menu polls and reflects the service state (interval, and
  whether to add `/api/health`).
- Mechanism for batch-job yielding in the transcriber.
- Module layout (`dictation.rs` controller, `overlay.rs`, `rawkeys.rs`,
  `paste.rs`, `dictation_client.rs`) and how Detect shares `rawkeys.rs`.
- Test plan: pure controller, tap detector, overlay maths, config parsing,
  path/listing exclusion, client against a stub; `#[ignore]` for hardware.

## Calls made after the builder check (mine; the user can overrule)

- **Fn / HID:** v1 is keyboard Raw Input only (see Triggers). Binding Fn works
  only when the keyboard reports it that way; true vendor-HID Fn is a follow-up.
- **Same key for both gestures** (user's decision, replaces my earlier rule that
  rejected overlap): hold and double-tap both default to Right Alt, resolved by
  press duration as described under Triggers. Residual risks: Right Alt is AltGr
  on many layouts (needs the fake-Ctrl handling) and a lone Alt press can
  activate menus (needs the suppression trick).
- **Engine sharing:** `/api/dictate` uses the same loaded engine, under a lock
  or gate taken per piece. Whistle with `workers` > 1 may use its pool. A batch
  job is not cancelled mid-piece, so worst-case wait is one piece (a Bark piece
  of up to 20 s ≈ 5 s). Accepted for v1.
- **Mic and detector:** the plan includes a verification step that the
  mic-usage exclusion covers the tray exe (installed and dev paths), and that a
  second shared-mode `CaptureStream` on the same device works during a recording.
- **Raw Input window:** `rawkeys.rs` owns its own message-only window (tray and
  Detect each create one), not the tray's existing window.
- **Clipboard:** restore after a fixed ~500 ms; set the clipboard-history and
  cloud-sync exclusion formats while the dictated text is on it, so it does not
  land in Win+V history.
- **Stop from the tray when the server was started elsewhere:** if the API
  answers, `/api/shutdown` works for any starter; if it does not answer and the
  tray has no process it started, show a toast ("couldn't stop it") and do nothing.
- **Timeouts:** the tray's transcribe request waits up to 10 minutes; Esc cancels.
- **On by default:** `dictation.enabled = true` with Right Alt for both
  gestures (user's decision). Raw Input failing to register produces a toast as
  well as a log line.

## Decisions that are mine, not the user's

Overlay approach, paste approach, Raw Input, upload-bytes endpoint, minimum hold
300 ms, Esc to cancel, 5 min cap. The user decided: the double-tap does not
stop the server, and Right Alt is the default for both hold and double-tap.
