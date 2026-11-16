// Gains for the two decoded channels so the chosen speakers play in both ears.

export type Listen = 'all' | 'me' | 'others'

export function listenGains(speakers: string[], listen: Listen): [number, number] {
  // Mono: always play the one channel (Listen is hidden, so a stale choice must not mute it).
  if (speakers.length === 1) return [1, 0]
  const gain = (ch: number) => {
    const who = speakers[ch]
    if (who === undefined) return 0 // mono: channel 1 does not exist
    return listen === 'all' || listen === who ? 1 : 0
  }
  return [gain(0), gain(1)]
}
