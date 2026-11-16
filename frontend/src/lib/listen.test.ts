import { describe, expect, it } from 'vitest'
import { listenGains } from './listen'

describe('listenGains', () => {
  it('plays the chosen speakers', () => {
    expect(listenGains(['me', 'others'], 'all')).toEqual([1, 1])
    expect(listenGains(['me', 'others'], 'me')).toEqual([1, 0])
    expect(listenGains(['me', 'others'], 'others')).toEqual([0, 1])
  })
  it('silences the missing second channel of a mono file', () => {
    expect(listenGains(['me'], 'all')).toEqual([1, 0])
  })
  it('always plays a mono file, whatever was picked for a stereo one', () => {
    expect(listenGains(['me'], 'others')).toEqual([1, 0])
    expect(listenGains(['others'], 'me')).toEqual([1, 0])
  })
  it('follows the channel map, not a fixed order', () => {
    expect(listenGains(['others', 'me'], 'me')).toEqual([0, 1])
  })
})
