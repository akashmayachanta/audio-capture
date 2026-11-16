/** `m:ss`, or `h:mm:ss` from an hour up. */
export function fmtTime(secs: number): string {
  const s = Number.isFinite(secs) ? Math.max(0, Math.floor(secs)) : 0
  const ss = String(s % 60).padStart(2, '0')
  if (s >= 3600) return `${Math.floor(s / 3600)}:${String(Math.floor(s / 60) % 60).padStart(2, '0')}:${ss}`
  return `${Math.floor(s / 60)}:${ss}`
}

export function fmtDuration(secs: number | null | undefined): string {
  return secs == null ? '' : fmtTime(secs)
}

function dayKey(d: Date): string {
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`
}

/** Consecutive items grouped by local day: "Today", "Yesterday", else a date. */
export function groupByDay<T extends { started_iso: string }>(items: T[], today: Date): { label: string; items: T[] }[] {
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1)
  const groups: { key: string; label: string; items: T[] }[] = []
  for (const item of items) {
    const d = new Date(item.started_iso)
    const key = dayKey(d)
    let g = groups[groups.length - 1]
    if (!g || g.key !== key) {
      const label =
        key === dayKey(today)
          ? 'Today'
          : key === dayKey(yesterday)
            ? 'Yesterday'
            : d.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' })
      g = { key, label, items: [] }
      groups.push(g)
    }
    g.items.push(item)
  }
  return groups.map(({ label, items }) => ({ label, items }))
}
