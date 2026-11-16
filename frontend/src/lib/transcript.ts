// Reads the transcript Markdown the transcriber writes next to a recording
// (format: backend/transcriber/app/markdown.py). Lines that are not transcript
// lines are ignored, so hand edits survive.

export type Speaker = 'me' | 'others'
export type Segment = { speaker: Speaker; start: number; text: string }
export type Transcript = { title: string; fields: Record<string, string>; segments: Segment[] }

const LINE = /^\*\*\[(\d+):(\d\d):(\d\d)\] (Me|Others):\*\* ?(.*)$/
const FIELD = /^- \*\*(.+?):\*\* (.*)$/

export function parseTranscript(md: string): Transcript {
  const t: Transcript = { title: '', fields: {}, segments: [] }
  for (const raw of md.split('\n')) {
    const line = raw.replace(/\r$/, '')
    let m: RegExpMatchArray | null
    if (line.startsWith('# ') && !t.title) {
      t.title = line.slice(2).replace(/^Transcript: /, '').trim()
    } else if ((m = line.match(FIELD))) {
      t.fields[m[1]] = m[2].replace(/^`+|`+$/g, '')
    } else if ((m = line.match(LINE))) {
      const [, h, mi, s, who, said] = m
      t.segments.push({
        speaker: who.toLowerCase() as Speaker,
        start: Number(h) * 3600 + Number(mi) * 60 + Number(s),
        text: said,
      })
    }
  }
  return t
}
