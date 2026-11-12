import { invoke } from '@tauri-apps/api/core'

import { isTauri } from '@/lib/platform'

/**
 * Local `qe-agent` daemon IPC adapter.
 *
 * The webview never touches the Unix socket; Tauri Rust commands in
 * `src-tauri/src/ipc.rs` perform the HTTP/1.1-over-UDS request and return the
 * parsed JSON. Keep this file the only place that knows command names.
 */

export const DEFAULT_IPC_SOCKET = '/run/qe-agent/agent.sock'

export interface AgentIntervals {
  fast_interval_secs: number
  full_interval_secs: number
}

/** Shape of `GET /v1/status` (see backend docs/agent-ipc.md). */
export interface AgentStatus {
  version: string
  roles: string[]
  report_mode: string | null
  enrolled: boolean
  device_slot: string | null
  serial: string | null
  mtls_endpoint: string | null
  data_dir: string | null
  socket: string
  started_at: number
  uptime_secs: number
  intervals: AgentIntervals | null
}

export interface AgentPolicyPayload {
  telemetry_fast_interval_secs?: number
  telemetry_full_interval_secs?: number
}

/** Shape of `GET /v1/policy`. */
export interface AgentPolicy {
  source: string
  revision: string | number | null
  policy: AgentPolicyPayload
}

/** Shape of `GET /v1/logs`. */
export interface PowerReply {
  ok: boolean
  data?: {
    action?: string
    fake?: boolean
    cleanup?: { vms?: string[]; containers?: string[] } | null
    scheduled?: boolean
    delay_secs?: number
    message?: string
  }
}

export interface AgentLogsReply {
  items: string[]
}

/** Shape of `POST /v1/enroll` (200 issued / 202 pending). */
export interface EnrollReply {
  status: string
  device_slot?: string
  serial?: string
  mtls_endpoint?: string
  enroll_id?: string
}

const BROWSER_ONLY_ERROR = '本机 IPC 仅在桌面端可用（当前为浏览器预览）。'

function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) return Promise.reject(new Error(BROWSER_ONLY_ERROR))
  return invoke<T>(command, args)
}

export function ipcStatus(): Promise<AgentStatus> {
  return call<AgentStatus>('agent_ipc_status')
}

export function ipcLogs(limit = 200): Promise<AgentLogsReply> {
  return call<AgentLogsReply>('agent_ipc_logs', { limit })
}

export function ipcPower(action: string, graceful = true): Promise<PowerReply> {
  return call<PowerReply>('agent_ipc_power', { action, graceful })
}

export function ipcPolicy(): Promise<AgentPolicy> {
  return call<AgentPolicy>('agent_ipc_policy')
}

export function ipcEnroll(link: string, trust = true, waitSecs = 300): Promise<EnrollReply> {
  return call<EnrollReply>('agent_ipc_enroll', { link, trust, waitSecs })
}

/** Accepted shapes: the daemon wants `https://…/enroll?token=…`, deep links may wrap it. */
export function isEnrollLink(value: string): boolean {
  try {
    const url = new URL(value.trim())
    return ['http:', 'https:', 'qestudio:', 'qestudio-agent:'].includes(url.protocol)
  } catch {
    return false
  }
}

/** Socket path shown by the UI; works in browser preview too (default only). */
export async function ipcEndpoint(): Promise<string> {
  if (!isTauri) return DEFAULT_IPC_SOCKET

  try {
    return await invoke<string>('agent_ipc_endpoint')
  } catch {
    return DEFAULT_IPC_SOCKET
  }
}
