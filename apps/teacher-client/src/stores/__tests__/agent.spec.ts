import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import {
  ipcEndpoint,
  ipcEnroll,
  ipcLogs,
  ipcPolicy,
  ipcStatus,
  type AgentLogsReply,
  type AgentPolicy,
  type AgentStatus,
  type EnrollReply,
} from '@/lib/ipc'
import { parseLogLine, useAgentStore } from '../agent'

vi.mock('@/lib/ipc', () => ({
  DEFAULT_IPC_SOCKET: '/run/qe-agent/agent.sock',
  ipcStatus: vi.fn<() => Promise<AgentStatus>>(),
  ipcLogs: vi.fn<() => Promise<AgentLogsReply>>(),
  ipcPolicy: vi.fn<() => Promise<AgentPolicy>>(),
  ipcEnroll: vi.fn<() => Promise<EnrollReply>>(),
  ipcEndpoint: vi.fn<() => Promise<string>>(),
}))

const status: AgentStatus = {
  version: '9.9.9',
  roles: ['node', 'machine'],
  report_mode: 'mtls',
  enrolled: true,
  device_slot: 'room-7',
  serial: '4f40cc',
  mtls_endpoint: 'https://qe.example:4441',
  data_dir: '/var/lib/qe-agent',
  socket: '/tmp/qe-agent-dev.sock',
  started_at: 1_763_000_000,
  uptime_secs: 3661,
  intervals: { fast_interval_secs: 10, full_interval_secs: 300 },
}

beforeEach(() => {
  vi.clearAllMocks()
  setActivePinia(createPinia())

  vi.mocked(ipcStatus).mockResolvedValue(status)
  vi.mocked(ipcLogs).mockResolvedValue({ items: [] })
  vi.mocked(ipcPolicy).mockResolvedValue({
    source: 'local',
    revision: null,
    policy: { telemetry_fast_interval_secs: 10, telemetry_full_interval_secs: 300 },
  })
  vi.mocked(ipcEndpoint).mockResolvedValue(status.socket)
  vi.mocked(ipcEnroll).mockResolvedValue({ status: 'issued' })
})

describe('agent store IPC wiring', () => {
  it('connects, exposes real status and adopts the socket path', async () => {
    const agent = useAgentStore()

    await agent.loadEndpoint()
    expect(agent.socketPath).toBe('/tmp/qe-agent-dev.sock')

    await expect(agent.ensureStatus()).resolves.toBe(true)
    expect(agent.connection).toBe('connected')
    expect(agent.connected).toBe(true)
    expect(agent.registered).toBe(true)
    expect(agent.status?.version).toBe('9.9.9')
    expect(agent.roles).toEqual(['node', 'machine'])
    expect(agent.socketPath).toBe('/tmp/qe-agent-dev.sock')
    expect(agent.error).toBeNull()
  })

  it('marks the daemon disconnected with a readable error', async () => {
    vi.mocked(ipcStatus).mockRejectedValue(
      new Error('未连接 qe-agent daemon：IPC socket 不存在（/run/qe-agent/agent.sock）'),
    )
    const agent = useAgentStore()

    await expect(agent.ensureStatus()).resolves.toBe(false)
    expect(agent.connection).toBe('disconnected')
    expect(agent.connected).toBe(false)
    expect(agent.status).toBeNull()
    expect(agent.error).toContain('未连接 qe-agent daemon')
  })

  it('parses tracing log lines and falls back to raw lines', async () => {
    vi.mocked(ipcLogs).mockResolvedValue({
      items: [
        '2026-11-01T03:19:22Z  INFO qe_agent::run: 上报成功 round=1',
        'plain line without tracing metadata',
      ],
    })
    const agent = useAgentStore()

    await expect(agent.refreshLogs()).resolves.toBe(true)
    expect(agent.logs).toHaveLength(2)
    expect(agent.logs[0]).toMatchObject({
      time: '03:19:22',
      level: 'INFO',
      message: 'qe_agent::run: 上报成功 round=1',
    })
    expect(agent.logs[1]).toMatchObject({
      level: 'LOG',
      message: 'plain line without tracing metadata',
    })
  })

  it('disconnects and surfaces log errors', async () => {
    vi.mocked(ipcLogs).mockRejectedValue(new Error('qe-agent 响应超时'))
    const agent = useAgentStore()

    await expect(agent.refreshLogs()).resolves.toBe(false)
    expect(agent.logs).toHaveLength(0)
    expect(agent.logsError).toContain('超时')
    expect(agent.connection).toBe('disconnected')
  })

  it('maps an issued enroll reply to the issued outcome and refreshes status', async () => {
    vi.mocked(ipcEnroll).mockResolvedValue({
      status: 'issued',
      device_slot: 'room-7',
      serial: '4f40cc',
      mtls_endpoint: 'https://qe.example:4441',
    } satisfies EnrollReply)
    const agent = useAgentStore()

    const outcome = await agent.enroll('https://qe.example/enroll?token=t', 120)

    expect(outcome.kind).toBe('issued')
    expect(agent.enrollOutcome?.kind).toBe('issued')
    expect(agent.enrolling).toBe(false)
    expect(ipcEnroll).toHaveBeenCalledWith('https://qe.example/enroll?token=t', true, 120)
    expect(ipcStatus).toHaveBeenCalled()
  })

  it('keeps a pending approval outcome', async () => {
    vi.mocked(ipcEnroll).mockResolvedValue({ status: 'pending', enroll_id: 'e-1' })
    const agent = useAgentStore()

    const outcome = await agent.enroll('https://qe.example/enroll?token=t')

    expect(outcome).toMatchObject({ kind: 'pending', reply: { enroll_id: 'e-1' } })
  })

  it('reports enroll failures with the daemon message', async () => {
    vi.mocked(ipcEnroll).mockRejectedValue(new Error('qe-agent 返回 HTTP 400：注册链接无效'))
    const agent = useAgentStore()

    const outcome = await agent.enroll('https://qe.example/enroll?token=bad')

    expect(outcome).toMatchObject({
      kind: 'failed',
      message: expect.stringContaining('注册链接无效'),
    })
    expect(agent.enrolling).toBe(false)
  })

  it('falls back to /v1/policy intervals when status has none', async () => {
    vi.mocked(ipcStatus).mockResolvedValue({ ...status, intervals: null })
    const agent = useAgentStore()

    await agent.ensureStatus()
    expect(agent.intervals).toBeNull()

    await agent.refreshPolicy()
    expect(agent.intervals).toEqual({ fast_interval_secs: 10, full_interval_secs: 300 })
  })

  it('stores deep links received from the OS bridge', () => {
    const agent = useAgentStore()
    agent.receiveDeepLink('qestudio://enroll?token=t')
    expect(agent.lastDeepLink).toBe('qestudio://enroll?token=t')
  })
})

describe('log line parsing', () => {
  it('extracts the short timestamp and level from tracing output', () => {
    expect(parseLogLine('2026-11-01T11:02:03Z  WARN qe_agent::ipc: slow request')).toMatchObject({
      time: '11:02:03',
      level: 'WARN',
      message: 'qe_agent::ipc: slow request',
    })
  })

  it('accepts debug and trace levels', () => {
    expect(parseLogLine('2026-11-01T11:02:03Z DEBUG qe_agent::x: y').level).toBe('DEBUG')
    expect(parseLogLine('2026-11-01T11:02:03Z TRACE qe_agent::x: y').level).toBe('TRACE')
  })
})
