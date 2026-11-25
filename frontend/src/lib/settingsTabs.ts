// Settings is split into tabs; the tab is the hash sub-path (#/settings/dictation),
// so the tray or a link can open one directly.

export type Tab = 'recording' | 'dictation' | 'transcription'

export const TABS: { id: Tab; label: string }[] = [
  { id: 'recording', label: 'Recording' },
  { id: 'dictation', label: 'Dictation' },
  { id: 'transcription', label: 'Transcription' },
]

export function tabFromHash(hash: string): Tab {
  const sub = hash.startsWith('#/settings/') ? hash.slice('#/settings/'.length) : ''
  return TABS.find((t) => t.id === sub)?.id ?? 'recording'
}

/** The tab each top-level config key is edited on; the rest is Recording. */
const TAB_OF: Record<string, Tab> = { dictation: 'dictation', speech_api: 'transcription' }

/** Tabs with unsaved edits (a dot on the tab). */
export function dirtyTabs(saved: object | null, edited: object | null): Set<Tab> {
  const out = new Set<Tab>()
  if (!saved || !edited) return out
  const a = saved as Record<string, unknown>
  const b = edited as Record<string, unknown>
  for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
    if (JSON.stringify(a[key]) !== JSON.stringify(b[key])) out.add(TAB_OF[key] ?? 'recording')
  }
  return out
}
