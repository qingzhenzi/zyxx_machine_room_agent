import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import { FALLBACK_APP_VERSION, appVersion } from '@/lib/app'
import {
  DEFAULT_IPC_SOCKET,
  ipcEndpoint,
  ipcEnroll,
  ipcLogs,
  ipcPolicy,
  ipcStatus,
  type AgentPolicy,
  type AgentStatus,
  type EnrollReply,
} from '@/lib/ipc'

export type AgentConnection = 'unknown' | 'connected' | 'disconnected'
export type AgentLogLevel = 'TRACE' | 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'LOG'

export interface AgentLogLine {
  id: number
  raw: string
  /** Short HH:mm:ss extracted from the tracing timestamp; empty when unknown. */
  time: string
  level: AgentLogLevel
  message: string
}

export type EnrollOutcome =
  | { kind: 'issued'; reply: EnrollReply }
  | { kind: 'pending'; reply: EnrollReply }
  | { kind: 'failed'; message: string }

const STATUS_POLL_MS = 2000
const LOGS_POLL_MS = 2000
const LOG_LIMIT = 200

let logSequence = 0

const LOG_LEVELS: readonly AgentLogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']

/** Parse one tracing-formatted line, falling back to the raw text. */
export function parseLogLine(raw: string): AgentLogLine {
  logSequence += 1
  const match = raw.match(/^(\d{4}-\d{2}-\d{2}T\S+)\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s+(.*)$/)

  if (!match) {
    return { id: logSequence, raw, time: '', level: 'LOG', message: raw }
  }

  const timestamp = match[1] ?? ''
  const level = (match[2] ?? 'INFO') as AgentLogLevel
  return {
    id: logSequence,
    raw,
    time: timestamp.slice(11, 19),
    level,
    message: match[3] ?? '',
  }
}

export function isAgentLogLevel(value: string): value is AgentLogLevel {
  return (LOG_LEVELS as readonly string[]).includes(value)
}

function errorMessage(caught: unknown): string {
  if (caught instanceof Error) return caught.message
  if (typeof caught === 'string') return caught
  return String(caught)
}

export const useAgentStore = defineStore('agent', () => {
  const connection = ref<AgentConnection>('unknown')
  const status = ref<AgentStatus | null>(null)
  const logs = ref<AgentLogLine[]>([])
  const policy = ref<AgentPolicy | null>(null)
  const socketPath = ref(DEFAULT_IPC_SOCKET)
  const error = ref<string | null>(null)
  const logsError = ref<string | null>(null)
  const lastUpdated = ref<number | null>(null)
  const enrolling = ref(false)
  const enrollOutcome = ref<EnrollOutcome | null>(null)
  const lastDeepLink = ref('')
  const fallbackVersion = ref(FALLBACK_APP_VERSION)

  let statusInFlight = false

  const registered = computed(() => status.value?.enrolled === true)
  const roles = computed(() => status.value?.roles ?? [])
  const version = computed(() => status.value?.version ?? fallbackVersion.value)
  const connected = computed(() => connection.value === 'connected')

  const intervals = computed(() => {
    if (status.value?.intervals) return status.value.intervals
    const payload = policy.value?.policy
    if (!payload) return null
    const fast = payload.telemetry_fast_interval_secs
    const full = payload.telemetry_full_interval_secs
    return fast !== undefined && full !== undefined
      ? { fast_interval_secs: fast, full_interval_secs: full }
      : null
  })

  void appVersion().then((value) => {
    fallbackVersion.value = value
  })

  async function loadEndpoint() {
    socketPath.value = await ipcEndpoint()
  }

  /** Refresh `/v1/status`; a failure means the daemon is unreachable. */
  async function ensureStatus(): Promise<boolean> {
    if (statusInFlight) return connected.value
    statusInFlight = true

    try {
      const next = await ipcStatus()
      status.value = next
      connection.value = 'connected'
      error.value = null
      lastUpdated.value = Date.now()
      if (next.socket) socketPath.value = next.socket
      return true
    } catch (caught) {
      status.value = null
      connection.value = 'disconnected'
      error.value = errorMessage(caught)
      return false
    } finally {
      statusInFlight = false
    }
  }

  async function refreshLogs(limit = LOG_LIMIT): Promise<boolean> {
    try {
      const reply = await ipcLogs(limit)
      logs.value = (reply.items ?? []).map(parseLogLine)
      logsError.value = null
      return true
    } catch (caught) {
      logs.value = []
      logsError.value = errorMessage(caught)
      connection.value = 'disconnected'
      error.value = logsError.value
      return false
    }
  }

  async function refreshPolicy(): Promise<boolean> {
    try {
      policy.value = await ipcPolicy()
      return true
    } catch {
      policy.value = null
      return false
    }
  }

  /** Call `POST /v1/enroll`; the daemon blocks until issued or approval pending. */
  async function enroll(link: string, waitSecs = 300): Promise<EnrollOutcome> {
    enrolling.value = true
    enrollOutcome.value = null

    try {
      const reply = await ipcEnroll(link, true, waitSecs)
      enrollOutcome.value =
        reply.status === 'pending'
          ? { kind: 'pending', reply }
          : { kind: 'issued', reply }
      await ensureStatus()
      return enrollOutcome.value
    } catch (caught) {
      enrollOutcome.value = { kind: 'failed', message: errorMessage(caught) }
      return enrollOutcome.value
    } finally {
      enrolling.value = false
    }
  }

  function receiveDeepLink(url: string) {
    lastDeepLink.value = url
  }

  function resetEnrollOutcome() {
    enrollOutcome.value = null
  }

  /** Poll `/v1/status` every 2s (the status page requirement is global). */
  function startStatusPolling(intervalMs = STATUS_POLL_MS): () => void {
    void ensureStatus()
    const timer = setInterval(() => {
      void ensureStatus()
    }, intervalMs)
    return () => clearInterval(timer)
  }

  /** Poll `/v1/logs` every 2s while the logs page is open. */
  function startLogsPolling(intervalMs = LOGS_POLL_MS): () => void {
    void refreshLogs()
    const timer = setInterval(() => {
      void refreshLogs()
    }, intervalMs)
    return () => clearInterval(timer)
  }

  return {
    connection,
    status,
    logs,
    policy,
    socketPath,
    error,
    logsError,
    lastUpdated,
    enrolling,
    enrollOutcome,
    lastDeepLink,
    registered,
    roles,
    version,
    connected,
    intervals,
    loadEndpoint,
    ensureStatus,
    refreshLogs,
    refreshPolicy,
    enroll,
    receiveDeepLink,
    resetEnrollOutcome,
    startStatusPolling,
    startLogsPolling,
  }
})
