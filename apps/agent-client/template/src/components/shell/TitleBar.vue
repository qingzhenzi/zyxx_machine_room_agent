<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Moon, Search, Sun } from 'lucide-vue-next'

import BrandMark from '@/components/ui/BrandMark.vue'
import WindowControls from '@/components/shell/WindowControls.vue'
import { useDesktopWindow } from '@/composables/useDesktopWindow'
import { appConfig } from '@/config/app'
import { modKey } from '@/lib/platform'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const settings = useSettingsStore()
const ui = useUiStore()
const { desktop, maximized, minimize, toggleMaximize, close } = useDesktopWindow()

const themeIcon = computed(() => (settings.resolvedTheme === 'dark' ? Sun : Moon))
const themeLabel = computed(() =>
  settings.resolvedTheme === 'dark' ? t('titlebar.themeToLight') : t('titlebar.themeToDark'),
)

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
      <span data-tauri-drag-region class="truncate text-[12.5px] font-semibold tracking-[-0.01em] text-ink">
        {{ appConfig.name }}
      </span>
      <span class="mono rounded border border-line px-1 py-px text-[9px] leading-[13px] text-faint">
        v{{ appConfig.version }}
      </span>
    </div>

    <div data-tauri-drag-region class="flex min-w-0 flex-1 justify-center">
      <button
        type="button"
        class="no-drag flex h-[24px] w-full max-w-[360px] items-center gap-2 rounded-[4px] border border-line bg-canvas px-2.5 text-[11px] text-faint transition-colors hover:border-line-strong hover:text-muted"
        @click="ui.openCommand()"
      >
        <Search :size="12" />
        <span class="flex-1 truncate text-left">{{ t('titlebar.searchPlaceholder') }}</span>
        <span class="flex items-center gap-0.5">
          <kbd class="kbd">{{ modKey }}</kbd>
          <kbd class="kbd">K</kbd>
        </span>
      </button>
    </div>

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
