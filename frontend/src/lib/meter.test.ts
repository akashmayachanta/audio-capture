import { describe, expect, it } from 'vitest'
import { levelDb, levelPct } from './meter'

describe('meter', () => {
  it('maps the 0..1 level scale to -50..0 dBFS', () => {
    expect(levelDb(0)).toBe(-50)
    expect(levelDb(0.3)).toBe(-35)
    expect(levelDb(1)).toBe(0)
  })
  it('gives a bar width in percent, clamped', () => {
    expect(levelPct(0.42)).toBe(42)
    expect(levelPct(-1)).toBe(0)
    expect(levelPct(2)).toBe(100)
  })
})
