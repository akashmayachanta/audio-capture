import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.fn()
vi.mock('./api', () => ({ transcriber: { state: () => state() } }))

const { createTranscriber } = await import('./transcriber.svelte')

const ok = { recordings_dir: '', models: [], engine: { active: null, loaded: null, loading: null, error: '', load_secs: null, device: '' }, jobs: [] }

describe('createTranscriber polling', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    state.mockReset()
  })
  afterEach(() => vi.useRealTimers())

  it('never runs two requests at once', async () => {
    let release!: () => void
    state.mockImplementation(() => new Promise((r) => (release = () => r(ok))))
    const tx = createTranscriber(2000)
    const a = tx.poll()
    const b = tx.poll()
    expect(state).toHaveBeenCalledTimes(1)
    release()
    await Promise.all([a, b])
    expect(tx.current.online).toBe(true)
  })

  it('waits for a slow reply before scheduling the next poll', async () => {
    let release!: () => void
    state.mockImplementation(() => new Promise((r) => (release = () => r(ok))))
    const tx = createTranscriber(2000)
    tx.start()
    expect(state).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(10_000) // reply still pending
    expect(state).toHaveBeenCalledTimes(1)
    release()
    await vi.advanceTimersByTimeAsync(2000)
    expect(state).toHaveBeenCalledTimes(2)
    tx.stop()
  })

  it('stops polling after stop()', async () => {
    state.mockResolvedValue(ok)
    const tx = createTranscriber(2000)
    tx.start()
    await vi.advanceTimersByTimeAsync(0)
    tx.stop()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(state).toHaveBeenCalledTimes(1)
  })
})
