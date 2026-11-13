import { computed, ref, watch } from 'vue'
import { defineStore } from 'pinia'

import type { Density, ThemeMode } from '@/lib/settings'
import { initialLocale, setLocale, type AppLocale } from '@/i18n'

export type { Density, ThemeMode } from '@/lib/settings'

interface PersistedSettings {
  locale: AppLocale
  theme: ThemeMode
  density: Density
  launchAtStartup: boolean
  startHidden: boolean
}

const STORAGE_KEY = 'qestudio.settings'

const defaults: PersistedSettings = {
  locale: initialLocale,
  theme: 'dark',
  density: 'compact',
  launchAtStartup: false,
  startHidden: true,
}

function loadSettings(): PersistedSettings {
  if (typeof window === 'undefined') return { ...defaults }

  try {
    const stored = JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? '{}') as Partial<PersistedSettings>
    return { ...defaults, ...stored }
  } catch {
    return { ...defaults }
  }
}

export const useSettingsStore = defineStore('settings', () => {
  const initial = loadSettings()

  const locale = ref<AppLocale>(initial.locale)
  const theme = ref<ThemeMode>(initial.theme)
  const density = ref<Density>(initial.density)
  const launchAtStartup = ref(initial.launchAtStartup)
  /**
   * P0.2: the window is always hidden on start (fixed in tauri.conf.json); this
   * preference only documents the future daemon behaviour and is stored locally.
   * TODO(P3): the `qe-agent` daemon decides whether to start hidden.
   */
  const startHidden = ref(initial.startHidden)

  const systemPrefersDark = ref(
    typeof window !== 'undefined' &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-color-scheme: dark)').matches,
  )

  const resolvedTheme = computed<'dark' | 'light'>(() =>
    theme.value === 'system' ? (systemPrefersDark.value ? 'dark' : 'light') : theme.value,
  )

  function applyTheme() {
    if (typeof document === 'undefined') return

    document.documentElement.dataset.theme = resolvedTheme.value
    document.documentElement.dataset.density = density.value
    document
      .querySelector('meta[name="theme-color"]')
      ?.setAttribute('content', resolvedTheme.value === 'dark' ? '#0c0e11' : '#ecebe6')
  }

  function persist() {
    if (typeof window === 'undefined') return

    try {
      const payload: PersistedSettings = {
        locale: locale.value,
        theme: theme.value,
        density: density.value,
        launchAtStartup: launchAtStartup.value,
        startHidden: startHidden.value,
      }
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(payload))
    } catch {
      // Storage can be unavailable in hardened webviews; the app still works in-memory.
    }
  }

  function setTheme(next: ThemeMode) {
    theme.value = next
  }

  function toggleTheme() {
    theme.value = resolvedTheme.value === 'dark' ? 'light' : 'dark'
  }

  function setDensity(next: Density) {
    density.value = next
  }

  function reset() {
    locale.value = defaults.locale
    theme.value = defaults.theme
    density.value = defaults.density
    launchAtStartup.value = defaults.launchAtStartup
    startHidden.value = defaults.startHidden
  }

  let mediaQuery: MediaQueryList | undefined

  function watchSystemTheme() {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function' || mediaQuery) {
      return
    }

    mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
    mediaQuery.addEventListener('change', (event) => {
      systemPrefersDark.value = event.matches
    })
  }

  watch(locale, (value) => setLocale(value), { immediate: true })
  watch(resolvedTheme, applyTheme, { immediate: true })
  watch(density, applyTheme)
  watch([locale, theme, density, launchAtStartup, startHidden], persist)

  return {
    locale,
    theme,
    density,
    launchAtStartup,
    startHidden,
    resolvedTheme,
    setTheme,
    toggleTheme,
    setDensity,
    reset,
    watchSystemTheme,
  }
})
