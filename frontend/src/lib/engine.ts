// Which service transcribes from the Recordings page: the local transcriber
// or the speech API set up in Settings (runs in the tray).

export type Engine = 'local' | 'api'

const KEY = 'audcap.engine'

/** The saved choice when it is usable; else the API when it is set up and the local transcriber isn't running. */
export function pickEngine(saved: string | null, apiConfigured: boolean, localOnline: boolean): Engine {
  if (saved === 'api' && apiConfigured) return 'api'
  if (saved === 'local') return 'local'
  return apiConfigured && !localOnline ? 'api' : 'local'
}

export function loadEngine(): string | null {
  try {
    return localStorage.getItem(KEY)
  } catch {
    return null
  }
}

export function saveEngine(e: Engine) {
  try {
    localStorage.setItem(KEY, e)
  } catch {
    // not kept; the default is used next time
  }
}
