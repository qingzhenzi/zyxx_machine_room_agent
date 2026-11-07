import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@tauri-apps/api/core'
import { asTemplate, pollDisplayState } from './ipc'

describe('asTemplate', () => {
  it('accepts known templates', () => {
    expect(asTemplate('progress')).toBe('progress')
    expect(asTemplate('message')).toBe('message')
    expect(asTemplate('enroll')).toBe('enroll')
  })

  it('falls back to boot on unknown', () => {
    expect(asTemplate('unknown-x')).toBe('boot')
    expect(asTemplate(undefined)).toBe('boot')
  })
})

describe('pollDisplayState', () => {
  it('parses a full directive', async () => {
    vi.mocked(invoke).mockResolvedValue({
      overall: 'booting',
      config_sufficient: false,
      display: {
        version: 3,
        template: 'enroll',
        params: { state: 'pending', detail: '等待审批' },
      },
    })
    const state = await pollDisplayState()
    expect(state.display.version).toBe(3)
    expect(state.display.template).toBe('enroll')
    expect(state.display.params).toEqual({ state: 'pending', detail: '等待审批' })
  })

  it('fills defaults for missing fields', async () => {
    vi.mocked(invoke).mockResolvedValue({})
    const state = await pollDisplayState()
    expect(state.overall).toBe('unavailable')
    expect(state.display.template).toBe('boot')
    expect(state.display.version).toBe(0)
  })
})
