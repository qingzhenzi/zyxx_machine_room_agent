<script setup lang="ts">
import { onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'

import AgentNav from '@/components/shell/AgentNav.vue'
import StatusBar from '@/components/shell/StatusBar.vue'
import TabStrip from '@/components/shell/TabStrip.vue'
import TitleBar from '@/components/shell/TitleBar.vue'
import ToastViewport from '@/components/shell/ToastViewport.vue'
import { initDesktopBridge, routeFromDeepLink, setAutostart } from '@/lib/desktop'
import { useAgentStore } from '@/stores/agent'
import { useUiStore } from '@/stores/ui'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'

const { t, locale } = useI18n()
const route = useRoute()
const router = useRouter()
const agent = useAgentStore()
const ui = useUiStore()
const settings = useSettingsStore()
const tabs = useTabsStore()

let disposeDesktop: (() => void) | undefined
let stopStatusPolling: (() => void) | undefined

onMounted(() => {
  settings.watchSystemTheme()
  void setAutostart(settings.launchAtStartup)
  void agent.loadEndpoint()
  // Status polling is global: status/title/status-bar stay fresh on any page.
  stopStatusPolling = agent.startStatusPolling()

  void initDesktopBridge({
    // The `deep-link` event from the Rust single-instance handler and OS URL
    // callbacks both land here; /enroll renders the URL from the agent store.
    openDeepLink: (url) => {
      agent.receiveDeepLink(url)
      const target = routeFromDeepLink(url)
      if (target) void router.push(target)
    },
    nativeMenu: (id) => {
      if (id === 'settings') void router.push('/settings')
      if (id === 'logs') void router.push('/logs')
    },
  }).then((dispose) => {
    disposeDesktop = dispose
  })
})

onUnmounted(() => {
  stopStatusPolling?.()
  disposeDesktop?.()
})

let daemonWasConnected = false
let lastDisconnectToast = 0
watch(
  () => agent.connected,
  (connected) => {
    if (connected) {
      void agent.refreshPolicy()
      daemonWasConnected = true
      return
    }
    if (daemonWasConnected && Date.now() - lastDisconnectToast > 30_000) {
      lastDisconnectToast = Date.now()
      ui.pushToast({
        kind: 'warning',
        title: 'qe-agent daemon 已断开',
        detail: agent.error ?? 'IPC socket 不可用；请检查 systemctl status qe-agent',
      })
    }
    daemonWasConnected = false
  },
  { immediate: true },
)

watch(
  [() => route.path, () => route.meta.title, () => route.meta.icon],
  () => {
    tabs.open({
      path: route.path,
      title: route.meta.title ? t(route.meta.title) : t('状态'),
      icon: route.meta.icon ?? 'activity',
    })
    document.title = route.meta.title
      ? `${t(route.meta.title)} · QEStudio Agent`
      : 'QEStudio Agent'
  },
  { immediate: true },
)

function syncTabTitles() {
  tabs.rename('/', t('状态'))
  for (const tab of tabs.tabs) {
    if (tab.path === '/') continue
    const resolved = router.resolve(tab.path)
    if (resolved.meta.title) tabs.rename(tab.path, t(resolved.meta.title))
  }
  if (route.meta.title) document.title = `${t(route.meta.title)} · QEStudio Agent`
}

syncTabTitles()
watch(locale, syncTabTitles)
</script>

<template>
  <div class="grid h-dvh grid-rows-[38px_35px_minmax(0,1fr)_24px] overflow-hidden bg-canvas text-ink">
    <TitleBar />
    <TabStrip />

    <div class="grid min-h-0 grid-cols-[52px_minmax(0,1fr)]">
      <AgentNav />
      <main class="min-h-0 min-w-0 overflow-hidden">
        <RouterView v-slot="{ Component }">
          <Transition name="view-fade" mode="out-in">
            <component :is="Component" />
          </Transition>
        </RouterView>
      </main>
    </div>

    <StatusBar />

    <ToastViewport />
  </div>
</template>
