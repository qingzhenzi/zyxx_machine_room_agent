import type { ItemStatus } from '@/api/items'

export type Tone = 'success' | 'danger' | 'warning' | 'info' | 'muted' | 'accent'

export const toneDotClass: Record<Tone, string> = {
  success: 'bg-success',
  danger: 'bg-danger',
  warning: 'bg-warning',
  info: 'bg-info',
  accent: 'bg-accent',
  muted: 'bg-faint',
}

export const itemStatusTone: Record<ItemStatus, Tone> = {
  todo: 'muted',
  doing: 'accent',
  done: 'success',
}
