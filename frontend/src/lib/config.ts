import type { Config } from './api'

/**
 * The Settings form after config.toml changed behind it (tray menu, text editor):
 * fields the user edited since `saved` keep their value, the rest take `fresh`.
 * Compared per top-level field, so an edited hotkey keeps the whole hotkeys group.
 */
export function rebase(saved: Config, form: Config, fresh: Config): Config {
  const out: Record<string, unknown> = structuredClone(fresh)
  for (const key of Object.keys(form) as (keyof Config)[]) {
    if (JSON.stringify(form[key]) !== JSON.stringify(saved[key])) out[key] = structuredClone(form[key])
  }
  return out as Config
}
