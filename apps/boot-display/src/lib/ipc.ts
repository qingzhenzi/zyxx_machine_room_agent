import { invoke } from '@tauri-apps/api/core'

export interface DisplayState {
  scene: 'boot' | 'progress' | 'idle'
  message: string
  detail: string
  progress: number | null
}

const SCENES = ['boot', 'progress', 'idle'] as const

function asScene(value: unknown): DisplayState['scene'] {
  return SCENES.includes(value as DisplayState['scene']) ? (value as DisplayState['scene']) : 'boot'
}

export async function pollDisplayState(): Promise<DisplayState> {
  const raw = await invoke<Partial<DisplayState>>('display_state')
  return {
    scene: asScene(raw.scene),
    message: raw.message ?? '',
    detail: raw.detail ?? '',
    progress: typeof raw.progress === 'number' ? raw.progress : null,
  }
}
