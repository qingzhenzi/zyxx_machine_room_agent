<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Command } from 'lucide-vue-next'
import { useRoute, useRouter } from 'vue-router'

import { modKey } from '@/lib/platform'
import { iconByName } from '@/lib/icons'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const ui = useUiStore()

const navItems = computed(() => [
  { label: t('nav.home'), path: '/', icon: 'home', shortcut: '1' },
  { label: t('nav.items'), path: '/items', icon: 'items', shortcut: '2' },
  { label: t('nav.uiKit'), path: '/ui-kit', icon: 'ui-kit', shortcut: '3' },
])

const settingsActive = computed(() => route.path === '/settings')

function navigate(path: string) {
  if (route.path !== path) void router.push(path)
}
</script>

<template>
  <aside
    class="flex min-h-0 flex-col items-center border-r border-line bg-surface py-2"
    :aria-label="t('nav.main')"
  >
    <div class="group relative mb-2 flex justify-center">
      <button
        type="button"
        class="flex h-8 w-8 items-center justify-center rounded-md text-muted transition-colors hover:bg-surface-2 hover:text-ink"
        :aria-label="t('titlebar.commandPalette')"
        @click="ui.openCommand()"
      >
        <Command :size="16" />
      </button>
      <span
        class="pointer-events-none absolute left-[46px] top-1/2 z-40 flex -translate-y-1/2 items-center gap-1.5 whitespace-nowrap rounded border border-line bg-surface-3 px-2 py-1 text-[10.5px] text-ink-soft opacity-0 shadow-lg transition-opacity group-hover:opacity-100"
      >
        {{ t('titlebar.commandPalette') }}
        <kbd class="kbd">{{ modKey }} K</kbd>
      </span>
    </div>

    <div class="mb-2 h-px w-6 bg-line" />

    <nav class="flex flex-1 flex-col items-center gap-1">
      <div v-for="item in navItems" :key="item.path" class="group relative flex justify-center">
        <button
          type="button"
          class="relative flex h-9 w-9 items-center justify-center rounded-md transition-colors"
          :class="
            route.path === item.path
              ? 'bg-accent-soft text-accent'
              : 'text-muted hover:bg-surface-2 hover:text-ink'
          "
          :aria-label="item.label"
          :aria-current="route.path === item.path ? 'page' : undefined"
          @click="navigate(item.path)"
        >
          <span
            v-if="route.path === item.path"
            class="absolute -left-2 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-r bg-accent"
          />
          <component :is="iconByName(item.icon)" :size="17" />
        </button>
        <span
          class="pointer-events-none absolute left-[46px] top-1/2 z-40 flex -translate-y-1/2 items-center gap-2 whitespace-nowrap rounded border border-line bg-surface-3 px-2 py-1 text-[10.5px] text-ink-soft opacity-0 shadow-lg transition-opacity group-hover:opacity-100"
        >
          {{ item.label }}
          <kbd class="kbd">{{ modKey }} {{ item.shortcut }}</kbd>
        </span>
      </div>
    </nav>

    <div class="mt-auto flex flex-col items-center gap-1">
      <div class="mb-1 h-px w-6 bg-line" />
      <div class="group relative flex justify-center">
        <button
          type="button"
          class="relative flex h-9 w-9 items-center justify-center rounded-md transition-colors"
          :class="
            settingsActive
              ? 'bg-accent-soft text-accent'
              : 'text-muted hover:bg-surface-2 hover:text-ink'
          "
          :aria-label="t('nav.settings')"
          @click="navigate('/settings')"
        >
          <span
            v-if="settingsActive"
            class="absolute -left-2 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-r bg-accent"
          />
          <component :is="iconByName('settings')" :size="17" />
        </button>
        <span
          class="pointer-events-none absolute left-[46px] top-1/2 z-40 flex -translate-y-1/2 items-center gap-2 whitespace-nowrap rounded border border-line bg-surface-3 px-2 py-1 text-[10.5px] text-ink-soft opacity-0 shadow-lg transition-opacity group-hover:opacity-100"
        >
          {{ t('nav.settings') }}
          <kbd class="kbd">{{ modKey }} 4</kbd>
        </span>
      </div>
    </div>
  </aside>
</template>
