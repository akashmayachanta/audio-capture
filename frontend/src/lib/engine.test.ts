import { describe, expect, it } from 'vitest'
import { pickEngine } from './engine'

describe('pickEngine', () => {
  it('keeps a usable saved choice', () => {
    expect(pickEngine('api', true, true)).toBe('api')
    expect(pickEngine('local', true, false)).toBe('local')
  })
  it('drops the API when it is not set up', () => {
    expect(pickEngine('api', false, false)).toBe('local')
  })
  it('defaults to the API only when the local transcriber is offline', () => {
    expect(pickEngine(null, true, false)).toBe('api')
    expect(pickEngine(null, true, true)).toBe('local')
    expect(pickEngine(null, false, false)).toBe('local')
  })
})
