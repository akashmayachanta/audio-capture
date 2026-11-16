import { describe, expect, it } from 'vitest'
import { fmtDuration, fmtTime, groupByDay } from './format'

describe('fmtTime', () => {
  it('is m:ss, or h:mm:ss from an hour up', () => {
    expect(fmtTime(0)).toBe('0:00')
    expect(fmtTime(65.9)).toBe('1:05')
    expect(fmtTime(3723)).toBe('1:02:03')
    expect(fmtTime(-1)).toBe('0:00')
    expect(fmtTime(NaN)).toBe('0:00')
  })
})

describe('fmtDuration', () => {
  it('is empty when unknown', () => {
    expect(fmtDuration(null)).toBe('')
    expect(fmtDuration(38.13)).toBe('0:38')
  })
})

describe('groupByDay', () => {
  const today = new Date(2026, 10, 16, 14, 0)
  // Built from local times so the test doesn't depend on the machine's timezone.
  const item = (d: number, h: number) => ({ started_iso: new Date(2026, 10, d, h, 0).toISOString() })

  it('labels today and yesterday and keeps order', () => {
    const items = [item(16, 11), item(16, 9), item(15, 16), item(13, 11)]
    const groups = groupByDay(items, today)
    expect(groups.map((g) => g.label.length > 0)).toEqual([true, true, true])
    expect(groups[0].label).toBe('Today')
    expect(groups[0].items).toEqual([items[0], items[1]])
    expect(groups[1].label).toBe('Yesterday')
    expect(groups[2].items).toEqual([items[3]])
    expect(groups[2].label).not.toBe('Today')
  })

  it('is empty for no items', () => {
    expect(groupByDay([], today)).toEqual([])
  })
})
