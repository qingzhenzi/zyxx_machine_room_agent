import { createI18n } from 'vue-i18n'

import { appConfig } from '@/config/app'
import enUS from './locales/en-US'
import zhCN from './locales/zh-CN'

export type AppLocale = 'zh-CN' | 'en-US'

export const localeOptions: Array<{ id: AppLocale; label: string }> = [
  { id: 'zh-CN', label: '简体中文' },
  { id: 'en-US', label: 'English' },
]

const SETTINGS_KEY = `${appConfig.storagePrefix}.settings`

function storedLocale(): AppLocale {
  if (typeof window === 'undefined') return 'zh-CN'
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY)
    if (raw) {
      const locale = (JSON.parse(raw) as { locale?: string }).locale
      if (locale === 'en-US' || locale === 'zh-CN') return locale
    }
  } catch {
    // Fall through to the default locale.
  }
  return 'zh-CN'
}

export const initialLocale = storedLocale()

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: 'zh-CN',
  messages: {
    'zh-CN': zhCN,
    'en-US': enUS,
  },
  missingWarn: false,
  fallbackWarn: false,
})

if (typeof document !== 'undefined') document.documentElement.lang = initialLocale

export function setLocale(locale: AppLocale): void {
  i18n.global.locale.value = locale
  if (typeof document !== 'undefined') document.documentElement.lang = locale
}

export function currentLocale(): AppLocale {
  return i18n.global.locale.value as AppLocale
}

/** Translate outside a component setup (stores, exports, native menus). */
export function translate(key: string, params?: Record<string, unknown>): string {
  return params ? i18n.global.t(key, params) : i18n.global.t(key)
}
