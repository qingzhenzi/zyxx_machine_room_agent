import { createI18n } from 'vue-i18n'

import enUS from './locales/en-US'

export type AppLocale = 'zh-CN' | 'en-US'

export const localeOptions: Array<{ id: AppLocale; label: string }> = [
  { id: 'zh-CN', label: '简体中文' },
  { id: 'en-US', label: 'English' },
]

const SETTINGS_KEY = 'qestudio.settings'

function storedLocale(): AppLocale {
  if (typeof window === 'undefined') return 'zh-CN'
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY)
    if (raw) {
      const locale = (JSON.parse(raw) as { locale?: string }).locale
      if (locale === 'en-US' || locale === 'zh-CN') return locale
    }
  } catch {
    // fall through
  }
  return 'zh-CN'
}

export const initialLocale = storedLocale()

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: 'zh-CN',
  messages: {
    'zh-CN': {
      '{count} 个结果': '{count} 个结果',
      // Keys with interpolation need an explicit zh-CN entry: vue-i18n renders
      // a missing message as the raw key and does not interpolate its params.
      '已提交注册请求（占位）：{server}': '已提交注册请求（占位）：{server}',
      '共 {count} 条': '共 {count} 条',
      '最近 {count} 行；日志保存在 daemon 内存环形缓冲区。':
        '最近 {count} 行；日志保存在 daemon 内存环形缓冲区。',
      '收到深链接：{url}': '收到深链接：{url}',
    },
    'en-US': enUS,
  },
  missingWarn: false,
  fallbackWarn: false,
})

export function setLocale(locale: AppLocale): void {
  i18n.global.locale.value = locale
  if (typeof document !== 'undefined') document.documentElement.lang = locale
}

export function currentLocale(): AppLocale {
  return i18n.global.locale.value as AppLocale
}

/** Translate outside a component setup (stores, exports, native notifications). */
export function translate(key: string, params?: Record<string, unknown>): string {
  return params ? i18n.global.t(key, params) : i18n.global.t(key)
}
