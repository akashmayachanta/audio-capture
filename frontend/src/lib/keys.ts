// Dictation key names for Settings. Ids are `KeyId` names from
// backend/recorder/src/dictation/keys.rs (`RightAlt`, `F13`, `vk:0x7B`); '' = off.

export type KeyOption = { id: string; label: string }

/** Keys that rarely type anything, so they are safe to hold or double-tap. */
export const COMMON_KEYS: KeyOption[] = [
  { id: 'RightAlt', label: 'Right Alt' },
  { id: 'RightCtrl', label: 'Right Ctrl' },
  { id: 'LeftAlt', label: 'Left Alt' },
  { id: 'CapsLock', label: 'Caps Lock' },
  { id: 'ScrollLock', label: 'Scroll Lock' },
  { id: 'Pause', label: 'Pause' },
  { id: 'Insert', label: 'Insert' },
  { id: 'Menu', label: 'Menu' },
  ...Array.from({ length: 12 }, (_, i) => ({ id: `F${i + 13}`, label: `F${i + 13}` })),
]

/** "Right Alt", "Key 0x7B", "Off"; same wording as `KeyId::display_name`. */
export function keyLabel(id: string): string {
  if (id === '') return 'Off'
  const common = COMMON_KEYS.find((k) => k.id.toLowerCase() === id.toLowerCase())
  if (common) return common.label
  if (id.toLowerCase().startsWith('vk:')) return `Key ${id.slice(3)}`
  return id.replace(/(?!^)([A-Z])/g, ' $1')
}

/** The choices of a key select: Off, the common keys, and the current key when it is neither. */
export function keyOptions(current: string): KeyOption[] {
  const options = [{ id: '', label: 'Off' }, ...COMMON_KEYS]
  if (!options.some((o) => o.id === current)) options.push({ id: current, label: keyLabel(current) })
  return options
}
