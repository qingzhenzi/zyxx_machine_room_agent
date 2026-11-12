import { invoke } from '@tauri-apps/api/core'

import { isTauri } from '@/lib/platform'

/** 安装/初始化向导的后端探测结果（见 src-tauri/src/setup.rs）。 */
export interface SetupProbe {
  platform: string
  systemd: boolean
  pkexec: string | null
  agent_bin: string | null
  unit_installed: boolean
  service_active: boolean
  service_enabled: boolean
  group_exists: boolean
  user_in_group: boolean
  current_user: string
  socket_path: string
  setup_script: string | null
}

export interface SetupResult {
  ok: boolean
  code: number | null
  command: string
  stdout: string
  stderr: string
}

const BROWSER_ONLY = '安装/初始化仅在桌面端可用（当前为浏览器预览）。'

function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) return Promise.reject(new Error(BROWSER_ONLY))
  return invoke<T>(command, args)
}

export function probeSetup(): Promise<SetupProbe> {
  return call<SetupProbe>('agent_setup_probe')
}

export interface AgentSetupOptions {
  /** 允许 agent 接收远程关机命令（写入 /etc/qe-agent.env）。 */
  allowPoweroff?: boolean
  /** 本地默认遥测间隔；控制面策略仍可覆盖。 */
  fastIntervalSecs?: number
  fullIntervalSecs?: number
  /** 毫秒级实时指标间隔；0 关闭本地采样，默认 500。 */
  liveIntervalMs?: number
  /** 是否把实时样本推送到中转；false 时只本地记录并随快照聚合提交。 */
  livePush?: boolean
}

export function installDaemon(
  agentBin: string,
  guiUser?: string,
  start = true,
  options: AgentSetupOptions = {},
): Promise<SetupResult> {
  return call<SetupResult>('agent_setup_install', {
    agentBin,
    guiUser,
    start,
    allowPoweroff: options.allowPoweroff,
    fastIntervalSecs: options.fastIntervalSecs,
    fullIntervalSecs: options.fullIntervalSecs,
    liveIntervalMs: options.liveIntervalMs,
    livePush: options.livePush,
  })
}

export function enrollDaemon(link: string): Promise<SetupResult> {
  return call<SetupResult>('agent_setup_enroll', { link })
}

export function serviceAction(action: 'start' | 'stop' | 'restart'): Promise<SetupResult> {
  return call<SetupResult>('agent_setup_service', { action })
}

/** 降级手动命令：浏览器预览/没有 pkexec 时展示给用户复制。 */
export function manualInstallCommand(probe: SetupProbe | null, agentBin: string): string {
  const script =
    probe?.setup_script && probe.setup_script !== 'embedded'
      ? probe.setup_script
      : '/usr/share/qe-agent-client/agent-setup.sh（或用包内 resources/agent-setup.sh）'
  const user = probe?.current_user ?? '$USER'
  return `sudo /bin/sh ${script} install --agent-bin ${agentBin || '/usr/local/bin/qe-agent'} --gui-user ${user} --start`
}
