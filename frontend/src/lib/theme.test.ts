import { describe, expect, it } from 'vitest'
import { nextTheme, parseTheme, resolveTheme, themeLabel } from './theme'

describe('parseTheme', () => {
  it('keeps known values', () => {
    expect(parseTheme('light')).toBe('light')
    expect(parseTheme('dark')).toBe('dark')
    expect(parseTheme('system')).toBe('system')
  })
  it('falls back to system', () => {
    expect(parseTheme(null)).toBe('system')
    expect(parseTheme('')).toBe('system')
    expect(parseTheme('blue')).toBe('system')
  })
})

describe('resolveTheme', () => {
  it('follows Windows for system', () => {
    expect(resolveTheme('system', true)).toBe('dark')
    expect(resolveTheme('system', false)).toBe('light')
  })
  it('ignores Windows for an explicit choice', () => {
    expect(resolveTheme('light', true)).toBe('light')
    expect(resolveTheme('dark', false)).toBe('dark')
  })
})

describe('nextTheme', () => {
  it('cycles system -> light -> dark -> system', () => {
    expect(nextTheme('system')).toBe('light')
    expect(nextTheme('light')).toBe('dark')
    expect(nextTheme('dark')).toBe('system')
  })
})

describe('themeLabel', () => {
  it('names each choice', () => {
    expect(themeLabel('system')).toBe('Theme: match Windows')
    expect(themeLabel('light')).toBe('Theme: light')
    expect(themeLabel('dark')).toBe('Theme: dark')
  })
})
