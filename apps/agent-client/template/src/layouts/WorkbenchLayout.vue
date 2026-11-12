<script setup lang="ts">
import { onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'

import ActivityBar from '@/components/shell/ActivityBar.vue'
import CommandPalette from '@/components/shell/CommandPalette.vue'
import StatusBar from '@/components/shell/StatusBar.vue'
import TabStrip from '@/components/shell/TabStrip.vue'
import TitleBar from '@/components/shell/TitleBar.vue'
import ToastViewport from '@/components/shell/ToastViewport.vue'
import { useAppShortcuts } from '@/composables/useAppShortcuts'
import { initDesktopBridge, routeFromDeepLink, setAutostart } from '@/lib/desktop'
import { appConfig } from '@/config/app'
import { useItemsStore } from '@/stores/items'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'

const { t, locale } = useI18n()
const route = useRoute()
const router = useRouter()
const tabs = useTabsStore()
const items = useItemsStore()
const settings = useSettingsStore()
const ui = useUiStore()

useAppShortcuts(router)

let disposeDesktop: (() => void) | undefined

onMounted(async () => {
  void setAutostart(settings.launchAtStartup)

  disposeDesktop = await initDesktopBridge({
    openCommandPalette: () => ui.openCommand(),
    openDeepLink: (url) => {
      const target = routeFromDeepLink(url)
      if (target) void router.push(target)
    },
    nativeMenu: (id) => {
      if (id === 'settings') void router.push('/settings')
      if (id === 'command-palette') ui.openCommand()
      if (id === 'new-item') void router.push('/items')
    },
  })

  void items.load()
})

onUnmounted(() => {
  disposeDesktop?.()
})

watch(
  [() => route.path, () => route.meta.title, () => route.meta.icon],
  () => {
    const title = route.meta.title ? t(route.meta.title) : appConfig.name
    tabs.open({
      path: route.path,
      title,
      icon: (route.meta.icon as string | undefined) ?? 'home',
    })
    document.title = `${title} · ${appConfig.name}`
  },
  { immediate: true },
)

function syncTabTitles() {
  tabs.rename('/', t('nav.home'))
  for (const tab of tabs.tabs) {
    if (tab.path === '/') continue
    const resolved = router.resolve(tab.path)
    if (resolved.meta.title) tabs.rename(tab.path, t(resolved.meta.title))
  }
  if (route.meta.title) {
    document.title = `${t(route.meta.title)} · ${appConfig.name}`
  }
}

watch(locale, syncTabTitles)
</script>

<template>
  <div class="grid h-dvh grid-rows-[38px_35px_minmax(0,1fr)_24px] overflow-hidden bg-canvas text-ink">
    <TitleBar />
    <TabStrip />

    <div class="grid min-h-0 grid-cols-[52px_minmax(0,1fr)]">
      <ActivityBar />
      <main class="min-h-0 min-w-0 overflow-hidden">
        <RouterView v-slot="{ Component }">
          <Transition name="view-fade" mode="out-in">
            <component :is="Component" />
          </Transition>
        </RouterView>
      </main>
    </div>

    <StatusBar />

    <CommandPalette />
    <ToastViewport />
  </div>
</template>
