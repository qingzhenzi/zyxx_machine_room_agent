import { computed, ref, watch } from 'vue'
import { defineStore } from 'pinia'

export interface WorkspaceTab {
  path: string
  title: string
  icon: string
  pinned?: boolean
}

const statusTab: WorkspaceTab = {
  path: '/',
  title: '状态',
  icon: 'activity',
  pinned: true,
}

/** Tabs restored from localStorage are limited to real routes of this shell. */
const KNOWN_PATHS = new Set(['/', '/enroll', '/logs', '/settings'])

const STORAGE_KEY = 'qestudio.tabs'

interface PersistedTabs {
  tabs: WorkspaceTab[]
  activePath: string
}

function defaultTabs(): PersistedTabs {
  return { tabs: [{ ...statusTab }], activePath: '/' }
}

function loadTabs(): PersistedTabs {
  if (typeof window === 'undefined') return defaultTabs()

  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return defaultTabs()

    const parsed = JSON.parse(raw) as Partial<PersistedTabs>
    if (!Array.isArray(parsed.tabs)) return defaultTabs()

    const valid = parsed.tabs.filter(
      (tab): tab is WorkspaceTab =>
        Boolean(tab) &&
        typeof tab.path === 'string' &&
        KNOWN_PATHS.has(tab.path) &&
        typeof tab.title === 'string' &&
        typeof tab.icon === 'string',
    )

    const tabs = [
      { ...statusTab },
      ...valid.filter((tab) => tab.path !== '/').map((tab) => ({ ...tab, pinned: false })),
    ].slice(0, 12)

    const activePath =
      typeof parsed.activePath === 'string' &&
      tabs.some((tab) => tab.path === parsed.activePath)
        ? parsed.activePath
        : '/'

    return { tabs, activePath }
  } catch {
    return defaultTabs()
  }
}

export const useTabsStore = defineStore('tabs', () => {
  const initial = loadTabs()
  const tabs = ref<WorkspaceTab[]>(initial.tabs)
  const activePath = ref(initial.activePath)

  const activeTab = computed(() => tabs.value.find((tab) => tab.path === activePath.value))

  function persist() {
    if (typeof window === 'undefined') return
    try {
      window.localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({ tabs: tabs.value, activePath: activePath.value }),
      )
    } catch {
      // Storage may be unavailable; tabs still work in-memory.
    }
  }

  watch([tabs, activePath], persist, { deep: true, flush: 'sync' })

  function open(tab: WorkspaceTab) {
    activePath.value = tab.path
    const existing = tabs.value.find((item) => item.path === tab.path)

    if (existing) {
      if (existing.title !== tab.title) existing.title = tab.title
      return
    }

    tabs.value.push({ ...tab })
  }

  function rename(path: string, title: string) {
    const tab = tabs.value.find((item) => item.path === path)
    if (tab && tab.title !== title) tab.title = title
  }

  function close(path: string): string | null {
    const target = tabs.value.find((tab) => tab.path === path)
    if (!target || target.pinned) return null

    const index = tabs.value.findIndex((tab) => tab.path === path)
    tabs.value.splice(index, 1)

    if (activePath.value !== path) return null

    const next = tabs.value[index] ?? tabs.value[index - 1] ?? tabs.value[0]
    activePath.value = next?.path ?? '/'
    return activePath.value
  }

  function closeActive(): string | null {
    return close(activePath.value)
  }

  function closeOthers(path: string) {
    tabs.value = tabs.value.filter((tab) => tab.pinned || tab.path === path)
    activePath.value = path
  }

  function closeAll() {
    tabs.value = tabs.value.filter((tab) => tab.pinned)
    activePath.value = '/'
  }

  return {
    tabs,
    activePath,
    activeTab,
    open,
    rename,
    close,
    closeActive,
    closeOthers,
    closeAll,
  }
})
