import { currentLocale } from '@/i18n'

export function formatNumber(value: number): string {
  return new Intl.NumberFormat(currentLocale()).format(value)
}

export function formatPercent(value: number, fractionDigits = 1): string {
  return `${value.toFixed(fractionDigits)}%`
}

export function formatLatency(value: number): string {
  return `${value} ms`
}

export function formatUptime(value: number): string {
  return `${value.toFixed(2)}%`
}

export function formatClock(date: Date): string {
  return date.toLocaleTimeString(currentLocale(), {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
  })
}

export function formatDate(date: Date): string {
  return date.toLocaleDateString(currentLocale(), {
    month: 'long',
    day: 'numeric',
    weekday: 'short',
  })
}

export function formatTimezone(date: Date): string {
  const offset = -date.getTimezoneOffset() / 60
  return `UTC${offset >= 0 ? '+' : ''}${offset}`
}

export function relativeFromNow(iso: string): string {
  const delta = Date.now() - new Date(iso).getTime()
  const minutes = Math.max(0, Math.round(delta / 60_000))
  const english = currentLocale() === 'en-US'
  if (minutes < 1) return english ? 'just now' : '刚刚'
  if (minutes < 60) return english ? `${minutes} min ago` : `${minutes} 分钟前`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return english ? `${hours} h ago` : `${hours} 小时前`
  return english ? `${Math.round(hours / 24)} d ago` : `${Math.round(hours / 24)} 天前`
}

export function formatDurationSeconds(value: number | null): string {
  if (value === null) return '—'
  const seconds = Math.max(0, Math.round(value))
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
}

export function formatClockFromIso(iso: string | null): string {
  if (!iso) return '—'
  return new Date(iso).toLocaleTimeString(currentLocale(), {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function formatDateTimeFromIso(iso: string | null): string {
  if (!iso) return '—'
  return new Date(iso).toLocaleString(currentLocale(), {
    month: 'long',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

export function formatElapsedFrom(startIso: string, atIso: string): string {
  const seconds = Math.max(0, Math.floor((new Date(atIso).getTime() - new Date(startIso).getTime()) / 1000))
  return `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`
}

export function formatShortDuration(value: number): string {
  const seconds = Math.max(0, Math.round(value))
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
}


export function chartDayLabels(): string[] {
  const english = currentLocale() === 'en-US'
  return english
    ? ['22', '23', '24', '25', '26', '27', 'Now']
    : ['22日', '23日', '24日', '25日', '26日', '27日', '今天']
}
