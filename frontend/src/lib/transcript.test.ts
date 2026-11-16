import { describe, expect, it } from 'vitest'
import { parseTranscript } from './transcript'

// Same layout as backend/transcriber/app/markdown.py `to_markdown`.
const SAMPLE = [
  '# Transcript: 2026-11-13 11:15 · Chrome',
  '',
  '- **Recording:** `2026-11-13_1115_Chrome.opus`',
  '- **Date:** 2026-11-13 11:15',
  '- **Apps:** Chrome',
  '- **Duration:** 1:02:05',
  '- **Transcribed with:** Cactus Whistle on CPU, 6.1x real time',
  '',
  '---',
  '',
  '**[00:00:12] Me:** Hello, can you hear me?',
  '',
  'a line I added by hand',
  '',
  '**[01:02:03] Others:** Yes, loud and clear.',
  '',
].join('\n')

describe('parseTranscript', () => {
  it('reads the title, fields and lines', () => {
    const t = parseTranscript(SAMPLE)
    expect(t.title).toBe('2026-11-13 11:15 · Chrome')
    expect(t.fields['Recording']).toBe('2026-11-13_1115_Chrome.opus')
    expect(t.fields['Duration']).toBe('1:02:05')
    expect(t.segments).toEqual([
      { speaker: 'me', start: 12, text: 'Hello, can you hear me?' },
      { speaker: 'others', start: 3723, text: 'Yes, loud and clear.' },
    ])
  })

  it('has no lines when no speech was detected', () => {
    expect(parseTranscript('# Transcript: x\n\n---\n\n_No speech detected._\n').segments).toEqual([])
  })

  it('reads hours beyond 9', () => {
    expect(parseTranscript('**[12:00:00] Me:** x').segments[0].start).toBe(43200)
  })

  it('never throws on empty input', () => {
    expect(parseTranscript('')).toEqual({ title: '', fields: {}, segments: [] })
  })

  it('accepts CRLF line endings', () => {
    expect(parseTranscript('# Transcript: a\r\n**[00:00:01] Others:** hi\r\n').segments).toEqual([
      { speaker: 'others', start: 1, text: 'hi' },
    ])
  })
})
