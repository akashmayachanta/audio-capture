// Reads the transcript Markdown the transcriber writes next to a recording
// (format: backend/transcriber/app/markdown.py). Lines that are not transcript
// lines are ignored, so hand edits survive.

export type Speaker = 'me' | 'others'
/** `speaker`: which side (the mic is "me", everyone else "others"); `name`: the label as written in the
 * transcript ("Me", "Others", or e.g. "Speaker" for a one-channel file sent to a Whisper Max server). */
export type Segment = { speaker: Speaker; name: string; start: number; text: string }
export type Transcript = { title: string; fields: Record<string, string>; segments: Segment[] }

const LINE = /^\*\*\[(\d+):(\d\d):(\d\d)\] ([^*:\n]+?):\*\* ?(.*)$/
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
        speaker: who === 'Me' ? 'me' : 'others',
        name: who,
        start: Number(h) * 3600 + Number(mi) * 60 + Number(s),
        text: said,
      })
    }
  }
  return t
}
