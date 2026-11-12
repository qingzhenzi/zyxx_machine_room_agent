<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity, Link2, ScrollText, Settings } from 'lucide-vue-next'
import { useRoute, useRouter } from 'vue-router'

import type { Component } from 'vue'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()

interface NavItem {
  label: string
  path: string
  icon: Component
}

const primaryItems = computed<NavItem[]>(() => [
  { label: t('状态'), path: '/', icon: Activity },
  { label: t('注册'), path: '/enroll', icon: Link2 },
  { label: t('日志'), path: '/logs', icon: ScrollText },
])

const settingsItem = computed<NavItem>(() => ({
  label: t('设置'),
  path: '/settings',
  icon: Settings,
}))

function navigate(path: string) {
  if (route.path !== path) void router.push(path)
}
</script>

<template>
  <aside
    class="flex min-h-0 flex-col items-center border-r border-line bg-surface py-2"
    :aria-label="t('主导航')"
  >
    <nav class="flex flex-1 flex-col items-center gap-1">
      <div v-for="item in primaryItems" :key="item.path" class="group relative flex justify-center">
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
          <component :is="item.icon" :size="17" />
        </button>
        <span
          class="pointer-events-none absolute left-[46px] top-1/2 z-40 -translate-y-1/2 whitespace-nowrap rounded border border-line bg-surface-3 px-2 py-1 text-[10.5px] text-ink-soft opacity-0 shadow-lg transition-opacity group-hover:opacity-100"
        >
          {{ item.label }}
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
            route.path === settingsItem.path
              ? 'bg-accent-soft text-accent'
              : 'text-muted hover:bg-surface-2 hover:text-ink'
          "
          :aria-label="settingsItem.label"
          :aria-current="route.path === settingsItem.path ? 'page' : undefined"
          @click="navigate(settingsItem.path)"
        >
          <span
            v-if="route.path === settingsItem.path"
            class="absolute -left-2 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-r bg-accent"
          />
          <component :is="settingsItem.icon" :size="17" />
        </button>
        <span
          class="pointer-events-none absolute left-[46px] top-1/2 z-40 -translate-y-1/2 whitespace-nowrap rounded border border-line bg-surface-3 px-2 py-1 text-[10.5px] text-ink-soft opacity-0 shadow-lg transition-opacity group-hover:opacity-100"
        >
          {{ settingsItem.label }}
        </span>
      </div>
    </div>
  </aside>
</template>
