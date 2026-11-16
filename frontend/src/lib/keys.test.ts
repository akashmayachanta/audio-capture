import { describe, expect, it } from 'vitest'
import { COMMON_KEYS, keyLabel, keyOptions } from './keys'

describe('COMMON_KEYS', () => {
  it('starts with Right Alt and has unique ids', () => {
    expect(COMMON_KEYS[0]).toEqual({ id: 'RightAlt', label: 'Right Alt' })
    expect(new Set(COMMON_KEYS.map((k) => k.id)).size).toBe(COMMON_KEYS.length)
  })
  it('lists the keys that rarely type anything, then F13 to F24', () => {
    expect(COMMON_KEYS.map((k) => k.id)).toEqual([
      'RightAlt',
      'RightCtrl',
      'LeftAlt',
      'CapsLock',
      'ScrollLock',
      'Pause',
      'Insert',
      'Menu',
      ...Array.from({ length: 12 }, (_, i) => `F${i + 13}`),
    ])
  })
})

describe('keyLabel', () => {
  it('names the common keys', () => {
    expect(keyLabel('RightAlt')).toBe('Right Alt')
    expect(keyLabel('CapsLock')).toBe('Caps Lock')
    expect(keyLabel('F13')).toBe('F13')
  })
  it('accepts config.toml spellings in any case', () => {
    expect(keyLabel('rightalt')).toBe('Right Alt')
  })
  it('spaces out named keys that are not in the list', () => {
    expect(keyLabel('LeftCtrl')).toBe('Left Ctrl')
    expect(keyLabel('F5')).toBe('F5')
  })
  it('shows other keys by their code', () => {
    expect(keyLabel('vk:0x7B')).toBe('Key 0x7B')
  })
  it('calls an empty key Off', () => {
    expect(keyLabel('')).toBe('Off')
  })
})

describe('keyOptions', () => {
  it('is Off plus the common keys', () => {
    const o = keyOptions('RightAlt')
    expect(o[0]).toEqual({ id: '', label: 'Off' })
    expect(o.slice(1)).toEqual(COMMON_KEYS)
  })
  it('adds the current key when it is a custom one', () => {
    const o = keyOptions('vk:0x7B')
    expect(o.at(-1)).toEqual({ id: 'vk:0x7B', label: 'Key 0x7B' })
    expect(o).toHaveLength(COMMON_KEYS.length + 2)
  })
  it('does not repeat Off or a common key', () => {
    expect(keyOptions('')).toHaveLength(COMMON_KEYS.length + 1)
    expect(keyOptions('F13')).toHaveLength(COMMON_KEYS.length + 1)
  })
})
