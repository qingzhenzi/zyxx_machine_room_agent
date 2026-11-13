<script setup lang="ts">
import type { Component } from 'vue'
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Copy,
  Layers,
  LayoutDashboard,
  ListChecks,
  Moon,
  RefreshCw,
  Search,
  Settings,
  Sun,
} from 'lucide-vue-next'
import { useRouter } from 'vue-router'

import { appConfig } from '@/config/app'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'

interface PaletteItem {
  id: string
  label: string
  hint?: string
  category: 'pages' | 'actions'
  icon: Component
  run: () => void
}

const { t } = useI18n()
const router = useRouter()
const ui = useUiStore()
const settings = useSettingsStore()

const query = ref('')
const selected = ref(0)
const inputRef = ref<HTMLInputElement | null>(null)

const themeActionLabel = computed(() =>
  settings.resolvedTheme === 'dark' ? t('titlebar.themeToLight') : t('titlebar.themeToDark'),
)

const themeActionIcon = computed(() => (settings.resolvedTheme === 'dark' ? Sun : Moon))

const pages = computed<PaletteItem[]>(() => [
  {
    id: 'page-home',
    label: t('nav.home'),
    hint: t('home.subtitle'),
    category: 'pages',
    icon: LayoutDashboard,
    run: () => void router.push('/'),
  },
  {
    id: 'page-items',
    label: t('nav.items'),
    hint: t('items.subtitle'),
    category: 'pages',
    icon: ListChecks,
    run: () => void router.push('/items'),
  },
  {
    id: 'page-ui-kit',
    label: t('nav.uiKit'),
    hint: t('uiKit.subtitle'),
    category: 'pages',
    icon: Layers,
    run: () => void router.push('/ui-kit'),
  },
  {
    id: 'page-settings',
    label: t('nav.settings'),
    hint: t('settings.subtitle'),
    category: 'pages',
    icon: Settings,
    run: () => void router.push('/settings'),
  },
])

function copyVersion() {
  const text = `${appConfig.name} v${appConfig.version}`
  if (!navigator.clipboard) {
    ui.pushToast({ kind: 'warning', title: t('palette.copyFailed') })
    return
  }

  void navigator.clipboard
    .writeText(text)
    .then(() => ui.pushToast({ kind: 'success', title: t('palette.copied') }))
    .catch(() => ui.pushToast({ kind: 'error', title: t('palette.copyFailed') }))
}

const actions = computed<PaletteItem[]>(() => [
  {
    id: 'action-new-item',
    label: t('palette.actionNewItem'),
    hint: t('items.formCreateTitle'),
    category: 'actions',
    icon: ListChecks,
    run: () => {
      void router.push({ path: '/items', query: { new: '1' } })
    },
  },
  {
    id: 'action-theme',
    label: themeActionLabel.value,
    hint: t('settings.theme'),
    category: 'actions',
    icon: themeActionIcon.value,
    run: () => settings.toggleTheme(),
  },
  {
    id: 'action-copy-version',
    label: t('common.version'),
    hint: `${appConfig.name} v${appConfig.version}`,
    category: 'actions',
    icon: Copy,
    run: copyVersion,
  },
  {
    id: 'action-reload',
    label: t('palette.actionReload'),
    hint: 'Ctrl/⌘ R',
    category: 'actions',
    icon: RefreshCw,
    run: () => window.location.reload(),
  },
])

const allItems = computed<PaletteItem[]>(() => [...pages.value, ...actions.value])

const filtered = computed(() => {
  const normalized = query.value.trim().toLowerCase()
  if (!normalized) return allItems.value

  return allItems.value.filter((item) =>
    `${item.label} ${item.hint ?? ''} ${t(`palette.group.${item.category}`)}`
      .toLowerCase()
      .includes(normalized),
  )
})

const groups = computed(() => {
  const order: PaletteItem['category'][] = ['pages', 'actions']
  let index = 0

  return order
    .map((category) => {
      const items = filtered.value
        .filter((item) => item.category === category)
        .map((item) => ({ item, index: index++ }))
      return { category, items }
    })
    .filter((group) => group.items.length > 0)
})

const flat = computed(() => groups.value.flatMap((group) => group.items.map((entry) => entry.item)))

watch(query, () => {
  selected.value = 0
})

watch(flat, (items) => {
  if (selected.value > items.length - 1) selected.value = Math.max(0, items.length - 1)
})

watch(selected, async (index) => {
  await nextTick()
  document.querySelector(`[data-palette-item="${index}"]`)?.scrollIntoView({ block: 'nearest' })
})

watch(
  () => ui.commandOpen,
  async (open) => {
    if (!open) return
    query.value = ''
    selected.value = 0
    await nextTick()
    inputRef.value?.focus()
  },
)

function runItem(item: PaletteItem) {
  ui.closeCommand()
  item.run()
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    if (flat.value.length > 0) selected.value = (selected.value + 1) % flat.value.length
    return
  }

  if (event.key === 'ArrowUp') {
    event.preventDefault()
    if (flat.value.length > 0) {
      selected.value = (selected.value - 1 + flat.value.length) % flat.value.length
    }
    return
  }

  if (event.key === 'Enter') {
    event.preventDefault()
    const item = flat.value[selected.value]
    if (item) runItem(item)
    return
  }

  if (event.key === 'Escape') {
    event.preventDefault()
    ui.closeCommand()
  }
}
</script>

<template>
  <Teleport to="body">
    <Transition name="palette">
      <div
        v-if="ui.commandOpen"
        class="fixed inset-0 z-50 flex items-start justify-center bg-black/45 px-4 pt-[11vh] backdrop-blur-[2px]"
        @mousedown.self="ui.closeCommand()"
      >
        <div
          class="panel w-full max-w-[620px] overflow-hidden shadow-[var(--shadow-float)]"
          role="dialog"
          aria-modal="true"
          :aria-label="t('titlebar.commandPalette')"
        >
          <div class="flex items-center gap-2.5 border-b border-line px-3.5">
            <Search :size="15" class="shrink-0 text-faint" />
            <input
              ref="inputRef"
              v-model="query"
              type="text"
              class="h-11 flex-1 bg-transparent text-[13px] text-ink placeholder:text-faint focus:outline-none"
              :placeholder="t('palette.placeholder')"
              autocomplete="off"
              spellcheck="false"
              @keydown="onKeydown"
            />
            <kbd class="kbd">esc</kbd>
          </div>

          <div class="max-h-[380px] overflow-y-auto p-1.5">
            <template v-for="group in groups" :key="group.category">
              <p class="section-label px-2.5 pb-1 pt-2">
                {{ t(`palette.group.${group.category}`) }}
              </p>
              <button
                v-for="entry in group.items"
                :key="entry.item.id"
                type="button"
                class="flex w-full items-center gap-2.5 rounded-[4px] border-l-2 px-2.5 py-[7px] text-left transition-colors"
                :class="
                  selected === entry.index
                    ? 'border-l-accent bg-accent-soft text-ink'
                    : 'border-l-transparent text-ink-soft hover:bg-surface-2'
                "
                :data-palette-item="entry.index"
                @mouseenter="selected = entry.index"
                @click="runItem(entry.item)"
              >
                <component
                  :is="entry.item.icon"
                  :size="14"
                  class="shrink-0"
                  :class="selected === entry.index ? 'text-accent' : 'text-faint'"
                />
                <span class="min-w-0 flex-1 truncate text-[12px]">{{ entry.item.label }}</span>
                <span class="max-w-[220px] truncate text-[10.5px] text-faint">
                  {{ entry.item.hint }}
                </span>
                <span v-if="selected === entry.index" class="shrink-0 text-[9px] text-faint">
                  ↵
                </span>
              </button>
            </template>

            <div
              v-if="flat.length === 0"
              class="flex flex-col items-center justify-center gap-1 py-10 text-center"
            >
              <Search :size="18" class="text-faint" />
              <p class="text-[12px] text-muted">{{ t('palette.emptyTitle') }}</p>
              <p class="text-[10.5px] text-faint">{{ t('palette.emptyHint') }}</p>
            </div>
          </div>

          <div
            class="flex items-center justify-between border-t border-line bg-surface-2 px-3 py-2 text-[10px] text-faint"
          >
            <div class="flex items-center gap-3">
              <span class="flex items-center gap-1">
                <kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd> {{ t('palette.select') }}
              </span>
              <span class="flex items-center gap-1">
                <kbd class="kbd">↵</kbd> {{ t('palette.open') }}
              </span>
              <span class="flex items-center gap-1">
                <kbd class="kbd">esc</kbd> {{ t('palette.close') }}
              </span>
            </div>
            <span class="mono">{{ t('palette.resultCount', { count: flat.length }) }}</span>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
