import { describe, expect, it } from 'vitest'
import { mergeRecordings, sameFolder, type Job, type LocalRecording } from './recordings'

const rec = (id: string): LocalRecording => ({
  id,
  file: id.split('/').pop()!,
  started: '2026-11-16 11:52',
  started_iso: '2026-11-16T11:52:00+05:30',
  duration_secs: 60,
  apps: ['Teams'],
  trigger: 'auto',
  status: 'complete',
  speakers: ['me', 'others'],
  transcribed: false,
  transcript_mtime: null,
})

const job = (id: number, rec_id: string, created: number, state: Job['state'] = 'done'): Job => ({
  id,
  rec_id,
  state,
  phase: '',
  progress: 0,
  error: '',
  created,
})

describe('mergeRecordings', () => {
  it('attaches the newest job per recording', () => {
    const rows = mergeRecordings([rec('a.opus'), rec('b.opus')], [job(1, 'a.opus', 10, 'error'), job(2, 'a.opus', 20, 'running')])
    expect(rows[0].job!.id).toBe(2)
    expect(rows[1].job).toBeNull()
    expect(rows.map((r) => r.id)).toEqual(['a.opus', 'b.opus'])
  })
})

describe('sameFolder', () => {
  it('ignores case, slash direction and a trailing slash', () => {
    expect(sameFolder('C:\\Users\\A\\Documents\\MeetRec\\', 'c:/users/a/documents/meetrec')).toBe(true)
  })
  it('tells different folders apart', () => {
    expect(sameFolder('C:\\Rec', 'C:\\Rec2')).toBe(false)
  })
})
