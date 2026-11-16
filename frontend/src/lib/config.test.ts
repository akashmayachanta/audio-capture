import { expect, test } from 'vitest'
import type { Config } from './api'
import { rebase } from './config'

const base: Config = {
  capture_mode: 'both',
  mic_device: '',
  system_device: '',
  output_dir: 'C:\\Rec',
  bitrate_kbps: 24,
  auto_record: true,
  trigger_apps: ['Teams', 'Zoom'],
  release_grace_secs: 10,
  min_duration_secs: 30,
  hotkeys: { start_stop: 'ctrl+alt+R', pause: 'ctrl+alt+P', split: 'ctrl+alt+S', marker: 'ctrl+alt+M' },
  start_with_windows: false,
  transcriber_dir: '',
}

test('an untouched form takes everything from the file', () => {
  const fresh: Config = { ...base, auto_record: false, capture_mode: 'mic' }
  expect(rebase(base, base, fresh)).toEqual(fresh)
})

test('fields the user edited win; the rest follow the file', () => {
  // Tray menu turned auto-record off while the form had an unsaved bitrate.
  const edited: Config = { ...base, bitrate_kbps: 32, trigger_apps: ['Teams'] }
  const fresh: Config = { ...base, auto_record: false }
  expect(rebase(base, edited, fresh)).toEqual({ ...base, auto_record: false, bitrate_kbps: 32, trigger_apps: ['Teams'] })
})

test('an edited hotkey keeps the whole hotkeys group', () => {
  const edited: Config = { ...base, hotkeys: { ...base.hotkeys, split: 'F9' } }
  const fresh: Config = { ...base, hotkeys: { ...base.hotkeys, marker: 'F10' }, start_with_windows: true }
  const merged = rebase(base, edited, fresh)
  expect(merged.hotkeys.split).toBe('F9')
  expect(merged.start_with_windows).toBe(true)
})

test('the result shares no objects with its inputs', () => {
  const merged = rebase(base, base, base)
  expect(merged.hotkeys).not.toBe(base.hotkeys)
  expect(merged.trigger_apps).not.toBe(base.trigger_apps)
})
