import { describe, expect, it } from 'vitest'
import { clampInt } from './numbers'

describe('clampInt', () => {
  it('keeps values in range, rounded', () => {
    expect(clampInt(24, 6, 128)).toBe(24)
    expect(clampInt(24.6, 6, 128)).toBe(25)
  })
  it('clamps out-of-range values instead of sending them to the server', () => {
    expect(clampInt(999, 6, 128)).toBe(128)
    expect(clampInt(-5, 1, 600)).toBe(1)
  })
  it('treats empty or garbage input as the minimum', () => {
    expect(clampInt('', 6, 128)).toBe(6)
    expect(clampInt(null, 0, 3600)).toBe(0)
    expect(clampInt(NaN, 1, 600)).toBe(1)
  })
})
