import { describe, expect, it } from 'vitest'

import {
  DEFAULT_IPC_SOCKET,
  ipcEndpoint,
  ipcPolicy,
  ipcStatus,
  isEnrollLink,
} from '../ipc'

describe('enrollment link validation', () => {
  it('accepts the server enrollment link and qestudio deep links', () => {
    expect(isEnrollLink('https://qe.example/enroll?token=t&slot=room-1')).toBe(true)
    expect(isEnrollLink('http://127.0.0.1:8080/enroll?token=t')).toBe(true)
    expect(isEnrollLink('qestudio://enroll?token=t')).toBe(true)
    expect(isEnrollLink('  https://qe.example/enroll?token=t  ')).toBe(true)
  })

  it('rejects empty, relative and unrelated values', () => {
    expect(isEnrollLink('')).toBe(false)
    expect(isEnrollLink('not a link')).toBe(false)
    expect(isEnrollLink('/enroll?token=t')).toBe(false)
    expect(isEnrollLink('ftp://qe.example/enroll')).toBe(false)
  })
})

describe('browser preview fallbacks', () => {
  it('exposes the default socket path without Tauri', async () => {
    await expect(ipcEndpoint()).resolves.toBe(DEFAULT_IPC_SOCKET)
  })

  it('rejects IPC calls with a readable message instead of throwing TypeError', async () => {
    await expect(ipcStatus()).rejects.toThrow('仅在桌面端可用')
    await expect(ipcPolicy()).rejects.toThrow('仅在桌面端可用')
  })
})
