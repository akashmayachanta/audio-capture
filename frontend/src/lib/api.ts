// Two backends: audcap.exe (same origin, always there) and the optional
// transcriber API on localhost.

import type { Job, LocalRecording } from './recordings'
import type { Launch } from './service'

export type CaptureMode = 'both' | 'mic' | 'system'

/** backend/recorder/src/config.rs `SpeechApi`: an OpenAI-compatible speech-to-text API. */
export type SpeechApi = { base_url: string; api_key: string; model: string }
export type PasteMode = 'live' | 'end'
export type DictationEngine = 'local' | 'api'
export type MicReady = 'off' | 'recent' | 'always'

/** backend/recorder/src/config.rs `Config`. */
export type Config = {
  capture_mode: CaptureMode
  mic_device: string
  system_device: string
  output_dir: string
  bitrate_kbps: number
  auto_record: boolean
  trigger_apps: string[]
  release_grace_secs: number
  min_duration_secs: number
  hotkeys: { start_stop: string; pause: string; split: string; marker: string }
  start_with_windows: boolean
  transcriber_dir: string
  /** Loopback port of the recordings API for other local apps; 0 = off. Not edited in Settings. */
  web_port: number
  speech_api: SpeechApi
  dictation: {
    enabled: boolean
    hold_key: string
    toggle_key: string
    keep_audio: boolean
    max_secs: number
    /** live = hands-free pastes each phrase as it is transcribed; end = everything when the dictation ends. */
    paste_mode: PasteMode
    /** local = the transcriber, phrase by phrase; api = `speech_api` (phrase by phrase when `api_phrases`). */
    engine: DictationEngine
    /** Speech must be this many dB above the room's noise floor (3–30). */
    noise_margin_db: number
    /** With the speech API: send each phrase while you keep talking (off = the whole clip at the end). */
    api_phrases: boolean
    /** Keep the mic open between dictations so their first words are not lost. */
    mic_ready: MicReady
    /** A pause only ends a phrase once it is this long (0–14 s; 0 = every pause). */
    min_phrase_secs: number
  }
}

/** backend/recorder/src/dictation/mic_test.rs `MicView`; levels on a 0..1 scale (-50..0 dBFS). */
export type MicView = {
  level: number
  floor: number | null
  threshold: number
  speech: boolean
  phrases: number
  last_phrase_secs: number | null
  secs: number
}

export type ConfigResponse = { config: Config; known_apps: string[] }
export type Device = { id: string; name: string }

export type Model = { id: string; label: string; description: string }
export type Engine = {
  active: string | null
  loaded: string | null
  loading: string | null
  error: string
  load_secs: number | null
  device: string
}

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
  ) {
    super(message)
  }
}

async function call<T>(url: string, init?: RequestInit): Promise<T> {
  const r = await fetch(url, init)
  if (!r.ok) {
    let message = r.statusText || `HTTP ${r.status}`
    try {
      const body = await r.json()
      message = body.error ?? body.detail ?? message
    } catch {
      // not JSON; keep the status text
    }
    throw new ApiError(message, r.status)
  }
  return r.json() as Promise<T>
}

const send = (method: string, body?: unknown): RequestInit => ({
  method,
  headers: { 'Content-Type': 'application/json' },
  body: body === undefined ? undefined : JSON.stringify(body),
})

export const local = {
  getConfig: () => call<ConfigResponse>('/api/config'),
  putConfig: (c: Config) => call<ConfigResponse>('/api/config', send('PUT', c)),
  devices: () => call<{ mics: Device[]; speakers: Device[] }>('/api/devices'),
  recordings: () => call<{ output_dir: string; recordings: LocalRecording[] }>('/api/recordings'),
  /** The transcript Markdown, or null when there is none. */
  async transcript(id: string): Promise<string | null> {
    const r = await fetch(`/api/transcript?id=${encodeURIComponent(id)}`)
    if (r.status === 404) return null
    if (!r.ok) throw new ApiError(`HTTP ${r.status}`, r.status)
    return r.text()
  },
  /** Deletes the transcript `.md`; the recording stays. */
  deleteTranscript: (id: string) => call<object>(`/api/transcript?id=${encodeURIComponent(id)}`, { method: 'DELETE' }),
  /** Deletes the recording with its sidecar and transcript. */
  deleteRecording: (id: string) => call<object>(`/api/recording?id=${encodeURIComponent(id)}`, { method: 'DELETE' }),
  openFolder: (id?: string) => call<object>('/api/open-folder', send('POST', { id: id ?? null })),
  pickFolder: () => call<{ path: string | null }>('/api/pick-folder', send('POST', {})),
  /** Starts `uv run python -m app` in the transcriber folder (hidden; it outlives the window). */
  startTranscriber: () => call<{ dir: string }>('/api/transcriber/start', send('POST', {})),
  transcriberLaunch: () => call<Launch>('/api/transcriber'),
  /** Tries the values as given (saved or not) with a second of silence. */
  speechTest: (api: SpeechApi) => call<{ ok: boolean; message: string; latency_ms: number }>('/api/speech/test', send('POST', api)),
  /** Speech-API jobs (they run in the tray, which this window asks). */
  speechJobs: () => call<{ jobs: Job[]; configured: boolean; model: string }>('/api/speech/jobs'),
  speechTranscribe: (id: string) => call<Job>('/api/speech/transcribe', send('POST', { id })),
  speechCancel: (jobId: number) => call<{ cancelled: boolean }>('/api/speech/cancel', send('POST', { job_id: jobId })),
  /** Waits up to 5 s for a key press; null when none came (or Esc was pressed). */
  /** Settings' mic test (window only): replaces a running one. It stops when not polled for 5 s. */
  micTestStart: (device: string, noise_margin_db: number) => call<{ ok: boolean }>('/api/mic-test', send('POST', { device, noise_margin_db })),
  micTest: () => call<{ test: MicView | null }>('/api/mic-test'),
  micTestStop: () => call<{ ok: boolean }>('/api/mic-test', { method: 'DELETE' }),
  detectKey: () => call<{ key: { id: string; name: string } | null }>('/api/detect-key', send('POST', {})),
}

export const audioUrl = (id: string) => `/api/audio?id=${encodeURIComponent(id)}`

export const TRANSCRIBER = 'http://127.0.0.1:8765'

export type TranscriberState = {
  recordings_dir: string
  models: Model[]
  engine: Engine
  jobs: Job[]
}

export const transcriber = {
  state: () => call<TranscriberState>(`${TRANSCRIBER}/api/state`, { signal: AbortSignal.timeout(3000) }),
  load: (model: string) => call<Engine>(`${TRANSCRIBER}/api/load`, send('POST', { model })),
  transcribe: (id: string) => call<Job>(`${TRANSCRIBER}/api/transcribe`, send('POST', { id })),
  shutdown: () => call<{ stopping: boolean }>(`${TRANSCRIBER}/api/shutdown`, send('POST', {})),
  cancel: (jobId: number) => call<{ cancelled: boolean }>(`${TRANSCRIBER}/api/cancel`, send('POST', { job_id: jobId })),
}
