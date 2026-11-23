// Start/Stop of the transcriber process from the window. The transcriber API
// being reachable is the truth; audcap.exe only knows about a process it
// started (to report one that exited right away).

/** audcap.exe `GET /api/transcriber`. */
export type Launch = { running: boolean; exit_code: number | null; log_tail: string; dir: string | null }

export type ServiceState = {
  pending: 'start' | 'stop' | null
  /** When `pending` began (ms). */
  since: number
  /** Why the last start/stop failed; shown until the transcriber is online. */
  failure: string
}

export type Phase = 'online' | 'offline' | 'starting' | 'stopping'

export const idle: ServiceState = { pending: null, since: 0, failure: '' }

/** A server that hasn't gone away this long after Stop is reported as stuck. */
export const STOP_TIMEOUT_MS = 15_000

export function failureText(l: Launch): string {
  const head = `The transcriber stopped (exit code ${l.exit_code}).`
  return l.log_tail ? `${head}\n${l.log_tail}` : head
}

/** Next state after a poll. No timeout for starting: the first start installs
 * the Python environment, which can take minutes. */
export function settle(s: ServiceState, online: boolean, launch: Launch | null, now: number): ServiceState {
  if (s.pending === 'start') {
    if (online) return idle
    if (launch && !launch.running && launch.exit_code !== null) return { ...idle, failure: failureText(launch) }
    return s
  }
  if (s.pending === 'stop') {
    if (!online) return idle
    if (now - s.since > STOP_TIMEOUT_MS) return { ...idle, failure: 'The transcriber did not stop. Close it from Task Manager (python.exe).' }
    return s
  }
  return online && s.failure ? idle : s
}

export function phase(s: ServiceState, online: boolean): Phase {
  if (s.pending === 'start') return 'starting'
  if (s.pending === 'stop') return 'stopping'
  return online ? 'online' : 'offline'
}
