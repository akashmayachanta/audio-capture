import { describe, expect, it } from 'vitest'
import { joinTriggerApps, splitTriggerApps } from './apps'

const KNOWN = ['Teams', 'Zoom', 'Webex', 'Slack', 'Chrome', 'Edge', 'Firefox']

describe('trigger apps', () => {
  it('turns known apps into toggles and the rest into custom text', () => {
    const { known, custom } = splitTriggerApps(['zoom', 'Teams', 'Discord', 'Signal'], KNOWN)
    expect(known).toHaveLength(KNOWN.length)
    expect(known.find((k) => k.name === 'Zoom')!.on).toBe(true)
    expect(known.find((k) => k.name === 'Teams')!.on).toBe(true)
    expect(known.find((k) => k.name === 'Chrome')!.on).toBe(false)
    expect(custom).toBe('Discord, Signal')
  })

  it('joins checked known apps, then custom names', () => {
    const known = [
      { name: 'Teams', on: true },
      { name: 'Zoom', on: false },
      { name: 'Chrome', on: true },
    ]
    expect(joinTriggerApps(known, ' Discord ,, Signal, ')).toEqual(['Teams', 'Chrome', 'Discord', 'Signal'])
  })

  it('round-trips', () => {
    const apps = ['Teams', 'Slack', 'Discord']
    const { known, custom } = splitTriggerApps(apps, KNOWN)
    expect(joinTriggerApps(known, custom)).toEqual(apps)
  })
})
