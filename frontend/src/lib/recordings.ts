// Recordings from meetrec.exe joined with transcription jobs from the transcriber.

/** `GET /api/recordings` item (backend/recorder/src/web_api/files.rs `RecordingJson`). */
export type LocalRecording = {
  id: string
  file: string
  started: string
  started_iso: string
  duration_secs: number | null
  apps: string[]
  trigger: 'auto' | 'manual' | null
  status: string
  speakers: string[]
  transcribed: boolean
  /** Changes when the transcript is redone. */
  transcript_mtime: number | null
}

export type Job = {
  id: number
  rec_id: string
  state: 'queued' | 'running' | 'done' | 'error' | 'cancelled'
  phase: string
  progress: number
  error: string
  created: number
}

export type Row = LocalRecording & { job: Job | null }

export function mergeRecordings(local: LocalRecording[], jobs: Job[]): Row[] {
  const latest = new Map<string, Job>()
  for (const j of jobs) {
    const prev = latest.get(j.rec_id)
    if (!prev || j.created >= prev.created) latest.set(j.rec_id, j)
  }
  return local.map((r) => ({ ...r, job: latest.get(r.id) ?? null }))
}

/** Same Windows folder: case-insensitive, either slash, trailing slash ignored. */
export function sameFolder(a: string, b: string): boolean {
  const norm = (p: string) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase()
  return norm(a) === norm(b)
}
