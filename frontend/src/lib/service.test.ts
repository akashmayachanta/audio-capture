import { describe, expect, it } from 'vitest'
import { failureText, idle, phase, settle, STOP_TIMEOUT_MS, type Launch } from './service'

const running: Launch = { running: true, exit_code: null, log_tail: '', dir: 'C:\\t' }
const exited = (code: number, log = ''): Launch => ({ running: false, exit_code: code, log_tail: log, dir: 'C:\\t' })
const starting = { pending: 'start' as const, since: 0, failure: '' }
const stopping = { pending: 'stop' as const, since: 0, failure: '' }

describe('service settle/phase', () => {
  it('follows the API when nothing is pending', () => {
    expect(phase(idle, true)).toBe('online')
    expect(phase(idle, false)).toBe('offline')
  })

  it('starting stays starting while the process runs, then goes online', () => {
    const s = settle(starting, false, running, 60_000)
    expect(phase(s, false)).toBe('starting')
    const on = settle(s, true, running, 61_000)
    expect(on.pending).toBeNull()
    expect(phase(on, true)).toBe('online')
  })

  it('a process that exits while starting is a failure with its log', () => {
    const s = settle(starting, false, exited(1, 'OSError: port in use'), 5000)
    expect(s.pending).toBeNull()
    expect(s.failure).toContain('exit code 1')
    expect(s.failure).toContain('port in use')
    expect(phase(s, false)).toBe('offline')
  })

  it('an unknown launch status keeps waiting', () => {
    expect(settle(starting, false, null, 5000).pending).toBe('start')
  })

  it('stopping ends when the API goes away, or gives up after a while', () => {
    expect(phase(settle(stopping, true, null, 1000), true)).toBe('stopping')
    expect(settle(stopping, false, null, 1000)).toEqual(idle)
    const late = settle(stopping, true, null, STOP_TIMEOUT_MS + 1)
    expect(late.pending).toBeNull()
    expect(late.failure).toMatch(/did not stop/)
  })

  it('a failure clears once the transcriber is online', () => {
    const failed = { pending: null, since: 0, failure: 'boom' }
    expect(settle(failed, false, null, 1).failure).toBe('boom')
    expect(settle(failed, true, null, 1).failure).toBe('')
  })

  it('failureText names the exit code and keeps the log', () => {
    expect(failureText(exited(2, 'a\nb'))).toBe('The transcriber stopped (exit code 2).\na\nb')
    expect(failureText(exited(2))).toBe('The transcriber stopped (exit code 2).')
  })
})
