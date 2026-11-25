// Light / dark theme: follow Windows (default) or pick one. Kept per window
// in localStorage; applied as `data-theme` on <html> (tokens in app.css).

export type Theme = 'system' | 'light' | 'dark'

const KEY = 'audcap.theme'
const DARK = '(prefers-color-scheme: dark)'

export function parseTheme(saved: string | null): Theme {
  return saved === 'light' || saved === 'dark' ? saved : 'system'
}

export function resolveTheme(t: Theme, systemDark: boolean): 'light' | 'dark' {
  return t === 'system' ? (systemDark ? 'dark' : 'light') : t
}

export function nextTheme(t: Theme): Theme {
  return t === 'system' ? 'light' : t === 'light' ? 'dark' : 'system'
}

export function themeLabel(t: Theme): string {
  return t === 'system' ? 'Theme: match Windows' : `Theme: ${t}`
}

export function loadTheme(): Theme {
  try {
    return parseTheme(localStorage.getItem(KEY))
  } catch {
    return 'system'
  }
}

export function saveTheme(t: Theme) {
  try {
    localStorage.setItem(KEY, t)
  } catch {
    // not kept; Windows' setting is used next time
  }
}

/** Sets `data-theme` now and, for `system`, whenever Windows switches. Returns the unsubscribe. */
export function applyTheme(t: Theme): () => void {
  const mq = window.matchMedia(DARK)
  const set = () => (document.documentElement.dataset.theme = resolveTheme(t, mq.matches))
  set()
  if (t !== 'system') return () => {}
  mq.addEventListener('change', set)
  return () => mq.removeEventListener('change', set)
}
