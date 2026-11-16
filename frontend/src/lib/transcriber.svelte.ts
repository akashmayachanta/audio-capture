// Polls the optional transcriber API. Going offline is a normal state, not an error.

import { local, transcriber, type TranscriberState } from './api'
import { idle, phase, settle, type Launch, type ServiceState } from './service'

export type { TranscriberState }

export type TState = {
  online: boolean
  /** Last state seen; kept while offline so the UI doesn't flicker. */
  state: TranscriberState | null
  /** Jobs that were queued/running last poll and are done now. */
  finishedJobIds: number[]
}

export type PollResult = { ok: true; state: TranscriberState } | { ok: false }

export const initial = (): TState => ({ online: false, state: null, finishedJobIds: [] })

export function reduce(prev: TState, result: PollResult): TState {
  if (!result.ok) return { ...prev, online: false, finishedJobIds: [] }
  const active = new Set((prev.state?.jobs ?? []).filter((j) => j.state === 'queued' || j.state === 'running').map((j) => j.id))
  const finishedJobIds = result.state.jobs.filter((j) => j.state === 'done' && active.has(j.id)).map((j) => j.id)
  return { online: true, state: result.state, finishedJobIds }
}

export function createTranscriber(intervalMs = 2000) {
  const s = $state<TState>(initial())
  const service = $state<ServiceState>({ ...idle })
  let timer: ReturnType<typeof setTimeout> | undefined
  let running = false
  let warned = false
  // One request at a time, so replies can't arrive out of order.
  let inFlight: Promise<void> | null = null

  async function fetchOnce() {
    let result: PollResult
    try {
      result = { ok: true, state: await transcriber.state() }
      warned = false
    } catch (e) {
      if (!warned) console.warn('transcriber offline:', e)
      warned = true
      result = { ok: false }
    }
    Object.assign(s, reduce(s, result))
    let launch: Launch | null = null
    if (service.pending === 'start') launch = await local.transcriberLaunch().catch(() => null)
    Object.assign(service, settle(service, s.online, launch, Date.now()))
  }

  function poll(): Promise<void> {
    inFlight ??= fetchOnce().finally(() => (inFlight = null))
    return inFlight
  }

  // The next poll is scheduled only after the previous reply (or timeout).
  async function loop() {
    await poll()
    if (running) timer = setTimeout(loop, intervalMs)
  }

  return {
    get current() {
      return s
    },
    /** Start/Stop of the transcriber process. */
    get service() {
      return { phase: phase(service, s.online), failure: service.failure }
    },
    async startService() {
      Object.assign(service, { pending: 'start', since: Date.now(), failure: '' })
      try {
        await local.startTranscriber()
      } catch (e) {
        Object.assign(service, { ...idle, failure: (e as Error).message })
        return
      }
      await poll()
    },
    async stopService() {
      Object.assign(service, { pending: 'stop', since: Date.now(), failure: '' })
      // The server may drop the connection as it exits; the polls tell.
      await transcriber.shutdown().catch(() => {})
      await poll()
    },
    poll,
    start() {
      running = true
      void loop()
    },
    stop() {
      running = false
      clearTimeout(timer)
    },
  }
}
