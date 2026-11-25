import { describe, expect, it } from 'vitest'
import { dirtyTabs, tabFromHash, TABS } from './settingsTabs'

describe('tabFromHash', () => {
  it('reads the sub-path', () => {
    expect(tabFromHash('#/settings/dictation')).toBe('dictation')
    expect(tabFromHash('#/settings/transcription')).toBe('transcription')
  })
  it('defaults to recording', () => {
    expect(tabFromHash('#/settings')).toBe('recording')
    expect(tabFromHash('#/settings/nope')).toBe('recording')
    expect(tabFromHash('')).toBe('recording')
  })
  it('lists every tab once', () => {
    expect(TABS.map((t) => t.id)).toEqual(['recording', 'dictation', 'transcription'])
  })
})

describe('dirtyTabs', () => {
  const base = {
    capture_mode: 'both',
    output_dir: 'C:/rec',
    hotkeys: { start_stop: 'ctrl+alt+R' },
    start_with_windows: true,
    dictation: { enabled: true, noise_margin_db: 10 },
    speech_api: { base_url: '', api_key: '', model: '' },
  }
  const edit = (f: (c: typeof base) => void) => {
    const c = structuredClone(base)
    f(c)
    return c
  }

  it('is empty when nothing changed', () => {
    expect(dirtyTabs(base, structuredClone(base))).toEqual(new Set())
  })
  it('puts each field under its tab', () => {
    expect(dirtyTabs(base, edit((c) => (c.dictation.noise_margin_db = 12)))).toEqual(new Set(['dictation']))
    expect(dirtyTabs(base, edit((c) => (c.speech_api.model = 'whisper-1')))).toEqual(new Set(['transcription']))
    expect(dirtyTabs(base, edit((c) => (c.hotkeys.start_stop = 'F9')))).toEqual(new Set(['recording']))
    expect(dirtyTabs(base, edit((c) => (c.start_with_windows = false)))).toEqual(new Set(['recording']))
  })
  it('handles a missing side', () => {
    expect(dirtyTabs(null, base)).toEqual(new Set())
  })
})
