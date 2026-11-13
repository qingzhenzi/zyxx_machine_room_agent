<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Info, Keyboard, Palette, Power } from 'lucide-vue-next'

import { appConfig } from '@/config/app'
import { setAutostart } from '@/lib/desktop'
import { isTauri, modKey } from '@/lib/platform'
import { usePlatformInfo } from '@/composables/usePlatformInfo'
import { localeOptions, type AppLocale } from '@/i18n'
import SegmentedControl from '@/components/ui/SegmentedControl.vue'
import ToggleSwitch from '@/components/ui/ToggleSwitch.vue'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const settings = useSettingsStore()
const ui = useUiStore()
const { info } = usePlatformInfo()

type SectionId = 'appearance' | 'startup' | 'shortcuts' | 'about'
const section = ref<SectionId>('appearance')

const sections = computed<Array<{ id: SectionId; label: string; icon: typeof Palette }>>(() => [
  { id: 'appearance', label: t('settings.section.appearance'), icon: Palette },
  { id: 'startup', label: t('settings.section.startup'), icon: Power },
  { id: 'shortcuts', label: t('settings.section.shortcuts'), icon: Keyboard },
  { id: 'about', label: t('settings.section.about'), icon: Info },
])

const themeOptions = computed(() => [
  { value: 'dark', label: t('settings.themeDark') },
  { value: 'light', label: t('settings.themeLight') },
  { value: 'system', label: t('settings.themeSystem') },
])

const densityOptions = computed(() => [
  { value: 'compact', label: t('settings.densityCompact') },
  { value: 'comfortable', label: t('settings.densityComfortable') },
])

const languageOptions = computed(() =>
  localeOptions.map((option) => ({ value: option.id, label: option.label })),
)

const shortcutRows = computed(() => [
  { keys: `${modKey} K`, label: t('shortcuts.commandPalette') },
  { keys: `${modKey} 1`, label: t('shortcuts.home') },
  { keys: `${modKey} 2`, label: t('shortcuts.items') },
  { keys: `${modKey} 4`, label: t('shortcuts.settings') },
])

async function toggleAutostart(enabled: boolean) {
  settings.launchAtStartup = enabled
  if (isTauri) await setAutostart(enabled)
}

function resetPreferences() {
  settings.reset()
  ui.pushToast({ kind: 'success', title: t('settings.resetDone') })
}
</script>

<template>
  <div class="flex h-full overflow-hidden">
    <aside class="w-[210px] shrink-0 border-r border-line bg-surface px-2.5 py-3">
      <h1 class="px-2 pb-2 text-[15px] font-semibold tracking-[-0.01em] text-ink">
        {{ t('settings.title') }}
      </h1>
      <nav class="space-y-0.5">
        <button
          v-for="item in sections"
          :key="item.id"
          type="button"
          class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[11.5px] transition-colors"
          :class="
            section === item.id
              ? 'bg-accent-soft text-accent'
              : 'text-muted hover:bg-surface-2 hover:text-ink'
          "
          :aria-current="section === item.id ? 'page' : undefined"
          @click="section = item.id"
        >
          <component :is="item.icon" :size="14" />
          {{ item.label }}
        </button>
      </nav>
    </aside>

    <div class="min-w-0 flex-1 overflow-y-auto px-5 py-4">
      <p class="mb-3 text-[11px] text-muted">{{ t('settings.subtitle') }}</p>

      <section v-if="section === 'appearance'" class="space-y-3">
        <article class="panel p-4">
          <h2 class="text-[12px] font-semibold text-ink">{{ t('settings.theme') }}</h2>
          <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.themeDescription') }}</p>
          <SegmentedControl
            class="mt-3 w-fit"
            :model-value="settings.theme"
            :options="themeOptions"
            :aria-label="t('settings.theme')"
            @update:model-value="settings.setTheme($event as 'dark' | 'light' | 'system')"
          />
        </article>

        <article class="panel p-4">
          <h2 class="text-[12px] font-semibold text-ink">{{ t('settings.density') }}</h2>
          <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.densityDescription') }}</p>
          <SegmentedControl
            class="mt-3 w-fit"
            :model-value="settings.density"
            :options="densityOptions"
            :aria-label="t('settings.density')"
            @update:model-value="settings.setDensity($event as 'compact' | 'comfortable')"
          />
        </article>

        <article class="panel p-4">
          <h2 class="text-[12px] font-semibold text-ink">{{ t('settings.language') }}</h2>
          <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.languageDescription') }}</p>
          <SegmentedControl
            class="mt-3 w-fit"
            :model-value="settings.locale"
            :options="languageOptions"
            :aria-label="t('settings.language')"
            @update:model-value="settings.locale = $event as AppLocale"
          />
        </article>
      </section>

      <section v-else-if="section === 'startup'" class="space-y-3">
        <article class="panel flex items-start justify-between gap-4 p-4">
          <div class="min-w-0">
            <h2 class="text-[12px] font-semibold text-ink">{{ t('settings.autostart') }}</h2>
            <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.autostartDescription') }}</p>
            <p v-if="!isTauri" class="mt-1 text-[10px] text-warning">
              {{ t('settings.autostartUnsupported') }}
            </p>
          </div>
          <ToggleSwitch
            :model-value="settings.launchAtStartup"
            :disabled="!isTauri"
            :label="t('settings.autostart')"
            @update:model-value="toggleAutostart"
          />
        </article>
      </section>

      <section v-else-if="section === 'shortcuts'" class="space-y-3">
        <article class="panel p-4">
          <ul class="divide-y divide-line">
            <li
              v-for="row in shortcutRows"
              :key="row.keys"
              class="flex items-center justify-between py-2 text-[11px] text-ink-soft"
            >
              <span>{{ row.label }}</span>
              <kbd class="kbd">{{ row.keys }}</kbd>
            </li>
          </ul>
          <p class="mt-3 text-[10px] leading-4 text-faint">{{ t('shortcuts.hint') }}</p>
        </article>
      </section>

      <section v-else class="space-y-3">
        <article class="panel p-4">
          <h2 class="text-[12px] font-semibold text-ink">{{ appConfig.name }}</h2>
          <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.aboutDescription') }}</p>

          <dl class="mt-3 grid grid-cols-2 gap-x-6 gap-y-2 text-[11px]">
            <div class="flex items-center justify-between border-b border-line pb-1.5">
              <dt class="text-muted">{{ t('common.version') }}</dt>
              <dd class="mono text-ink-soft">{{ info.appVersion }}</dd>
            </div>
            <div class="flex items-center justify-between border-b border-line pb-1.5">
              <dt class="text-muted">Tauri</dt>
              <dd class="mono text-ink-soft">{{ info.tauriVersion }}</dd>
            </div>
            <div class="flex items-center justify-between border-b border-line pb-1.5">
              <dt class="text-muted">{{ t('settings.platform') }}</dt>
              <dd class="mono text-ink-soft">{{ info.os }}</dd>
            </div>
            <div class="flex items-center justify-between border-b border-line pb-1.5">
              <dt class="text-muted">Arch</dt>
              <dd class="mono text-ink-soft">{{ info.arch }}</dd>
            </div>
          </dl>
        </article>

        <article class="panel flex items-start justify-between gap-4 p-4">
          <div class="min-w-0">
            <h2 class="text-[12px] font-semibold text-ink">{{ t('settings.reset') }}</h2>
            <p class="mt-0.5 text-[10px] text-muted">{{ t('settings.resetDescription') }}</p>
          </div>
          <button type="button" class="btn btn-sm shrink-0" @click="resetPreferences">
            {{ t('settings.reset') }}
          </button>
        </article>
      </section>
    </div>
  </div>
</template>
