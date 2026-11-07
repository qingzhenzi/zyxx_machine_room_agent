import { invoke } from '@tauri-apps/api/core'

/**
 * agent 下发的展示指令。模板由 agent 决定，boot-display 只渲染：
 * - boot      启动 logo + 文案
 * - progress  进度条（progress: 0-100 或 null 不确定动画）
 * - idle      待机时钟
 * - message   纯文本告示（任意 params.title / params.lines[]）
 * - enroll    注册流程状态（params.state: pending|failed|ok, params.detail）
 */
export interface DisplayDirective {
  version: number
  template: string
  params: Record<string, unknown>
}

export interface DisplayState {
  overall: 'booting' | 'ready' | 'degraded' | 'unavailable'
  config_sufficient: boolean
  display: DisplayDirective
}

const TEMPLATES = ['boot', 'progress', 'idle', 'message', 'enroll'] as const
export type TemplateName = (typeof TEMPLATES)[number]

export function asTemplate(value: unknown): TemplateName {
  return TEMPLATES.includes(value as TemplateName) ? (value as TemplateName) : 'boot'
}

export async function pollDisplayState(): Promise<DisplayState> {
  const raw = await invoke<Partial<DisplayState>>('display_state')
  return {
    overall: (raw.overall ?? 'unavailable') as DisplayState['overall'],
    config_sufficient: raw.config_sufficient ?? false,
    display: {
      version: raw.display?.version ?? 0,
      template: asTemplate(raw.display?.template),
      params: raw.display?.params ?? {},
    },
  }
}
