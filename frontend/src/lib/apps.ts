// The auto-record allow-list as known-app toggles plus free text.

export type KnownApp = { name: string; on: boolean }

export function splitTriggerApps(apps: string[], known: string[]): { known: KnownApp[]; custom: string } {
  const has = (name: string) => apps.some((a) => a.toLowerCase() === name.toLowerCase())
  return {
    known: known.map((name) => ({ name, on: has(name) })),
    custom: apps.filter((a) => !known.some((k) => k.toLowerCase() === a.toLowerCase())).join(', '),
  }
}

export function joinTriggerApps(known: KnownApp[], custom: string): string[] {
  const extra = custom
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
  return [...known.filter((k) => k.on).map((k) => k.name), ...extra]
}
