import { describe, expect, it } from 'vitest'
import { initial, reduce, type TranscriberState } from './transcriber.svelte'
import type { Job } from './recordings'

const job = (id: number, state: Job['state']): Job => ({ id, rec_id: 'a.opus', state, phase: '', progress: 0, error: '', created: id })

const state = (jobs: Job[]): TranscriberState => ({
  recordings_dir: 'C:\Rec',
  models: [],
  engine: { active: 'whistle', loaded: 'whistle', loading: null, error: '', load_secs: null, device: 'CPU' },
  jobs,
})

describe('transcriber reduce', () => {
  it('goes offline but keeps the last state', () => {
    const online = reduce(initial(), { ok: true, state: state([]) })
    const off = reduce(online, { ok: false })
    expect(off.online).toBe(false)
    expect(off.state).toEqual(online.state)
  })

  it('comes back online', () => {
    const off = reduce(initial(), { ok: false })
    expect(reduce(off, { ok: true, state: state([]) }).online).toBe(true)
  })

  it('reports jobs that just finished', () => {
    const running = reduce(initial(), { ok: true, state: state([job(1, 'running'), job(2, 'queued')]) })
    const next = reduce(running, { ok: true, state: state([job(1, 'done'), job(2, 'running')]) })
    expect(next.finishedJobIds).toEqual([1])
  })

  it('does not report jobs that were already done', () => {
    const done = reduce(initial(), { ok: true, state: state([job(1, 'done')]) })
    expect(done.finishedJobIds).toEqual([])
    expect(reduce(done, { ok: true, state: state([job(1, 'done')]) }).finishedJobIds).toEqual([])
  })
})
