import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@tauri-apps/api/core'
import { pollDisplayState } from './ipc'

describe('pollDisplayState', () => {
  it('normalizes a full state', async () => {
    vi.mocked(invoke).mockResolvedValue({
      scene: 'progress',
      message: '正在下载资源',
      detail: '3/10',
      progress: 30,
    })
    const state = await pollDisplayState()
    expect(state).toEqual({
      scene: 'progress',
      message: '正在下载资源',
      detail: '3/10',
      progress: 30,
    })
  })

  it('falls back to boot scene on unknown values', async () => {
    vi.mocked(invoke).mockResolvedValue({ scene: 'unknown', progress: 'x' })
    const state = await pollDisplayState()
    expect(state.scene).toBe('boot')
    expect(state.progress).toBeNull()
    expect(state.message).toBe('')
  })
})
