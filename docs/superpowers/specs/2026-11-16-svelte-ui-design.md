# AudCap: one Svelte app, frontend/backend split — design

Status: draft for approval (2026-11-16). Not committed (repo rule: the user commits).

## Intent

The egui window (`audcap.exe --settings` / `--recordings`) looks dated, and the
transcriber has its own separate page at `http://127.0.0.1:8765/` with a second
recordings list and a second player. Replace both with **one modern-looking
Svelte app** shown in a native window, and reorganize the repo into
`frontend/` and `backend/`, where backend holds the Rust exe and the Python
transcriber, which becomes an optional, API-only service.

## Goals

- One window for everything: recordings list, player (Both / Me / Others,
  click-a-timestamp-to-seek, playing line highlighted), transcript, Transcribe /
  Cancel, recorder Settings, transcription model Settings.
- Modern look: clean typography, cards, light/dark following Windows.
- Repo split: `frontend/`, `backend/recorder/`, `backend/transcriber/`.
- The transcriber is an optional API. The app fully works without it,
  including reading existing transcripts; only creating transcripts and
  loading models need it running.

## Non-goals

- Changing the tray, recorder, capture, encoder, controller, or detector.
- The exe starting/stopping the transcriber (still started with its
  `run.bat`). Possible later.
- Fixing autostart's stale Run-key path after the move (README note instead:
  toggle "Start with Windows" off and on once).
- Transcript editing, search, auth, installer, remembering window size/position.
- Making the transcriber URL configurable (constant `http://127.0.0.1:8765` for now).

## Anti-goals (failure even if it "works")

- The tray process loading WebView2 / any GUI stack (it stays a separate process,
  as egui is today).
- Settings, playback or reading transcripts breaking when the transcriber is
  not running.
- Needing Node/npm at runtime, or the page loading anything from the internet.
- Touching the user's real config/recordings in tests or live runs
  (`AUDCAP_CONFIG`, `AUDCAP_TRANSCRIBER_SETTINGS` scratch paths).

## Constraints

- Windows 11, WebView2 runtime (present on Win 11). If missing, the window
  process shows a message box with the error and exits.
- Node 26 / npm 11 are installed for building.
- Existing rules: no git commits; tests first for logic (`cargo test --lib`);
  no tests for the Python transcriber; prefer debug builds while iterating.

## Approaches considered

UI tech (decided earlier in the conversation):
- GPUI — dropped (no stock widgets, big exe and build cost).
- Slint / restyled egui — considered; user picked a web frontend in a wry window.
- Plain HTML/JS vs Svelte in that window — **user picked Svelte**.
- One app vs two pages — **user picked one app**.

Data path for the one app:
1. **Rust serves the app + local data (including transcript files); the page
   calls the transcriber API directly for jobs and models — picked.**
2. Rust proxies all transcriber calls — rejected: the exe would need an HTTP
   client and would mirror every transcriber route.
3. Python serves everything — rejected: Settings/playback would die with the
   transcriber, and Python can't enumerate WASAPI devices cleanly.

Transcripts while the transcriber is off: **shown** (user's call). The exe
serves the raw `.md`; the frontend parses it. The transcriber's
`/api/transcript` route is dropped, so there is one reader of the format (TS)
and one writer (Python `markdown.py`).

## Repo layout

```
frontend/                    Svelte 5 + Vite + TypeScript (SPA, no SvelteKit)
  package.json, vite.config.ts, src/…   →  builds to frontend/dist/
backend/
  recorder/                  the Rust crate → audcap.exe
    Cargo.toml, Cargo.lock, build.rs, src/, assets/
  transcriber/               the Python uv project (moved from transcriber/), API only
.cargo/config.toml           stays at the repo root (cargo finds it from below)
build.ps1                    npm ci + npm run build in frontend/, then cargo build
                             in backend/recorder/ (target dir moves with the crate)
docs/, README.md, CLAUDE.md  updated paths
```

## Backend: the window process (`backend/recorder`)

- `audcap.exe --settings` / `--recordings` (same mutex `Local\AudCap.Settings`)
  opens a **tao** window with a **wry** WebView. Starts on the matching page
  (`#/settings` or `#/recordings`). Resizable, default ~1100×720, min ~760×520.
  The tray menu items keep launching these args unchanged.
- WebView2 user data folder: `%LOCALAPPDATA%\AudCap\webview` (not next to the exe).
- Custom protocol `audcap` (WebView2 exposes it as `http://audcap.localhost/`):
  - `GET /…` — files from `frontend/dist`, embedded in the exe at build time
    (`include_dir`). If `dist/` is missing at build time, `build.rs` emits a
    warning and embeds a one-line placeholder page, so `cargo test --lib` and
    `cargo build` work without npm.
  - `GET /api/config` → `Config` as JSON (plus `known_apps`).
  - `PUT /api/config` → sanitize, save to `config.toml` (tray hot-reloads), return
    the saved config; 400 with a message on parse errors, 500 on write errors.
  - `GET /api/devices` → `{ mics: [{id,name}], speakers: [...] }` (enumerated on
    an MTA thread, as today).
  - `GET /api/recordings` → newest first: `id` (path relative to `output_dir`,
    forward slashes — the same id the transcriber uses), `file`, `started`,
    `duration_secs`, `apps`, `trigger`, `status`, `speakers` (per channel from
    the sidecar's channel map), `transcribed` (a `.md` exists).
  - `GET /api/transcript?id=` → the recording's `<name>.md` as `text/markdown`,
    404 if none (same id checks as audio).
  - `GET /api/audio?id=` → the `.opus` bytes, `audio/ogg`, honoring `Range`
    (206 + `Content-Range`) so seeking works. id must resolve to an `.opus`
    inside `output_dir` (no `..`, no absolute paths) or 404.
  - `POST /api/open-folder` (`{id?}`: the recording's folder, else `output_dir`)
    and `POST /api/pick-folder` → the `rfd` dialog, returns the chosen path or null.
- Routing, Range parsing, id → path resolution, and config load/save are pure
  functions in a `web_api` module, unit-tested without a window.
- Removed: `eframe` dependency, `settings_ui.rs`, `recordings_ui.rs`, and the
  in-process player `src/playback/` (`reader.rs`, `mix.rs`, `player.rs`) and its
  tests — WebView2 (Edge) decodes the family-1 files with both channels, and
  the Web Audio graph does the Both/Me/Others mix. `recordings.rs` stays and is
  extended for the JSON listing. `split_trigger_apps`/`join_trigger_apps` move
  to the frontend (with their tests).

## Backend: the transcriber API (`backend/transcriber`)

- API-only: delete `GET /` and `app/static/`; delete `GET /api/audio` and
  `GET /api/transcript` (the exe serves both now). `run.bat` stops opening a
  browser.
- CORS: allow origin `http://audcap.localhost` (GET/POST, `Content-Type`).
  The Host-header check stays.
- `/api/state`, `/api/load`, `/api/transcribe`, `/api/cancel` unchanged.
  Recording ids are already relative POSIX paths, so the page joins the two
  lists by id.
- Different folders: `/api/state` already returns `recordings_dir`. The
  frontend compares it (case-insensitive, normalized slashes) with the local
  `output_dir`; if they differ, Transcribe is disabled with a warning naming
  both folders.
- No new tests (repo rule).

## Frontend (`frontend/`)

- Svelte 5 (runes) + Vite + TypeScript, no UI component library; hand-written
  CSS with design tokens (colors, radius, spacing) and light/dark via
  `prefers-color-scheme`. System font stack (Segoe UI Variable). Tiny hash
  router (`#/recordings`, `#/settings`).
- Two API clients: `local` (same origin) and `transcriber`
  (`http://127.0.0.1:8765`). The transcriber client polls `/api/state` every
  2 s; a failed fetch flips the app to "Transcription offline" (no error spam,
  keeps polling). The local recordings list refreshes on window focus, on the
  Refresh button, and when the transcriber reports a job finished.
- Transcript parser in TS, ported from `markdown.py`'s `parse` (title, fields,
  segments with start/end/speaker/text), with Vitest cases written from the
  current format (including a real transcript file as a fixture).
- Content-Security-Policy meta: `default-src 'self'; connect-src 'self'
  http://127.0.0.1:8765; media-src 'self'`; no remote fonts/scripts.

Layout sketch:

```
┌──────────────────────────────────────────────────────────────────────┐
│ ● AudCap    Recordings  Settings          Transcription: Bark ready │
├───────────────────────┬──────────────────────────────────────────────┤
│ 🔍 (non-goal)  ⟳  📁  │ 2026-11-14 10:15 · Teams                     │
│ ┌───────────────────┐ │ 42:10 · auto · transcribed                   │
│ │ Today             │ │ ┌──────────────────────────────────────────┐ │
│ │ 10:15 Teams  42:10│ │ │ ▶  ━━━━━━━━●──────── 12:03 / 42:10  🔊   │ │
│ │       ✓ transcribed│ │ │ Listen  (Both) Me  Others                │ │
│ │ 09:00 Zoom   15:02│ │ └──────────────────────────────────────────┘ │
│ │       [Transcribe]│ │ Show (Both) Me Others                         │
│ │ Yesterday         │ │ 00:12  Me      Morning, can you hear me?     │
│ │ 16:30 Chrome  3:11│ │ 00:15  Others  Yes, loud and clear.  ◀ playing│
│ └───────────────────┘ │ …                                            │
└───────────────────────┴──────────────────────────────────────────────┘
```

- Recordings page: list grouped by day; each row shows time, app, duration, a
  transcript badge or a Transcribe / progress-bar / Cancel control (Transcribe
  hidden while the transcriber is offline or no model is loaded, with a hint).
  Detail pane: header, sticky custom player (play/pause, seek bar, time,
  volume, Listen Both/Me/Others hidden for mono), transcript with Show
  Both/Me/Others filter, clickable timestamps, playing line highlighted and
  kept in view. Not-yet-transcribed recordings still play.
- Settings page: cards for Capture (mode, mic, speakers), Storage (folder +
  Browse, bitrate per channel), Auto-record (toggle, known-app chips, other
  apps, grace secs, min duration), Hotkeys, Startup (start with Windows),
  Transcription (model list with status and Load buttons; disabled with a
  "start the transcriber" hint when offline).
- Transcribe controls when the transcriber is offline: hidden, with a hint;
  existing transcripts still open, filter and seek normally. Sticky Save / Discard bar appears
  only when the form is dirty; Save shows the error inline on failure.
- Player: `<audio src="/api/audio?id=…">` → `MediaElementSource` → splitter →
  per-channel gain → both ears (ported from the current transcriber page).
- Tests: Vitest for pure helpers (transcript parsing, trigger-app split/join,
  time formatting, merging local + transcriber recordings by id, folder
  mismatch check, day grouping, listen gains).

## Docs

README (build: Node is now needed; run: one window; transcriber is API-only;
autostart note after the move) and CLAUDE.md (new layout, file tables, routes,
build steps) updated.

## Left to the builder

Exact component split, CSS details and icons (inline SVG), crate versions
(wry 0.57, tao 0.37, include_dir), how build.ps1 reports npm failures.
