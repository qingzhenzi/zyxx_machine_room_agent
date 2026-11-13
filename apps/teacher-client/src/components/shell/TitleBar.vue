<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Moon, Sun } from 'lucide-vue-next'

import WindowControls from '@/components/shell/WindowControls.vue'
import BrandMark from '@/components/ui/BrandMark.vue'
import { useDesktopWindow } from '@/composables/useDesktopWindow'
import { useAgentStore } from '@/stores/agent'
import { useSettingsStore } from '@/stores/settings'

const { t } = useI18n()
const settings = useSettingsStore()
const agent = useAgentStore()
const { desktop, maximized, minimize, toggleMaximize, close } = useDesktopWindow()

const themeIcon = computed(() => (settings.resolvedTheme === 'dark' ? Sun : Moon))
const themeLabel = computed(() =>
  settings.resolvedTheme === 'dark' ? t('切换到浅色主题') : t('切换到深色主题'),
)

const connectionLabel = computed(() => {
  if (agent.connection === 'unknown') return t('连接中…')
  if (!agent.connected) return t('未连接')
  return agent.registered ? t('已注册') : t('未注册')
})

const connectionDot = computed(() => {
  if (agent.connection === 'unknown') return 'text-faint'
  if (!agent.connected) return 'text-danger'
  return agent.registered ? 'text-success' : 'text-warning'
})

function onTitlebarDoubleClick(event: MouseEvent) {
  if (!desktop) return
  if ((event.target as HTMLElement).closest('button, a, input')) return
  void toggleMaximize()
}
</script>

<template>
  <header
    data-tauri-drag-region
    class="relative flex h-[38px] select-none items-center gap-3 border-b border-line bg-surface pl-3 pr-0"
    @dblclick="onTitlebarDoubleClick"
  >
    <div data-tauri-drag-region class="flex min-w-0 shrink-0 items-center gap-2.5">
      <BrandMark :size="19" data-tauri-drag-region />
      <span data-tauri-drag-region class="text-[12.5px] font-semibold tracking-[-0.01em] text-ink">
        QEStudio Agent
      </span>
      <span class="mono rounded border border-line px-1 py-px text-[9px] leading-[13px] text-faint">
        AGENT
      </span>
      <span class="mx-1 h-4 w-px bg-line" />
      <span
        class="flex shrink-0 items-center gap-1.5 rounded border border-line bg-surface-2 px-1.5 py-[2px] text-[9.5px] text-muted"
      >
        <span class="status-dot" :class="connectionDot" />
        {{ connectionLabel }}
      </span>
    </div>

    <div data-tauri-drag-region class="min-w-0 flex-1" />

    <div class="no-drag flex shrink-0 items-center gap-1">
      <button
        type="button"
        class="icon-btn"
        :title="themeLabel"
        :aria-label="themeLabel"
        @click="settings.toggleTheme()"
      >
        <component :is="themeIcon" :size="14" />
      </button>

      <span class="mx-1 h-4 w-px bg-line" />

      <WindowControls
        :maximized="maximized"
        @minimize="minimize()"
        @maximize="toggleMaximize()"
        @close="close()"
      />
    </div>
  </header>
</template>
