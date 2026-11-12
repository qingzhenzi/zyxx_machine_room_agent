<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Cpu, EyeOff, Server, Wifi, WifiOff } from 'lucide-vue-next'

import StatusBadge from '@/components/ui/StatusBadge.vue'
import { useClock } from '@/composables/useClock'
import { useNetworkStatus } from '@/composables/useNetworkStatus'
import { useAgentStore } from '@/stores/agent'
import { useSettingsStore } from '@/stores/settings'

const { t } = useI18n()
const agent = useAgentStore()
const settings = useSettingsStore()
const { clockLabel, timezoneLabel } = useClock()
const { online } = useNetworkStatus()

const statusLabel = computed(() => {
  if (agent.connection === 'unknown') return t('连接中…')
  if (!agent.connected) return t('未连接')
  return agent.registered ? t('已注册') : t('未注册')
})

const statusTone = computed(() => {
  if (agent.connection === 'unknown') return 'muted'
  if (!agent.connected) return 'danger'
  return agent.registered ? 'success' : 'warning'
})

const roleLabel = computed(() => {
  if (!agent.connected) return t('未连接')
  if (agent.roles.length === 0) return '—'
  return agent.roles
    .map((role) =>
      role === 'node'
        ? t('节点（node）')
        : role === 'machine'
          ? t('机器（machine）')
          : role,
    )
    .join('+')
})
</script>

<template>
  <footer
    class="flex h-[24px] select-none items-center justify-between border-t border-line bg-surface px-3 text-[10.5px] text-muted"
  >
    <div class="flex min-w-0 items-center gap-3">
      <StatusBadge :label="statusLabel" :tone="statusTone" />
      <span class="h-3 w-px bg-line" />
      <span class="flex min-w-0 items-center gap-1.5" :title="agent.socketPath">
        <Server :size="11" class="shrink-0 text-faint" />
        <span class="mono max-w-[240px] truncate">{{ agent.socketPath }}</span>
      </span>
      <span class="h-3 w-px bg-line" />
      <span class="flex shrink-0 items-center gap-1.5">
        <Cpu :size="11" class="text-faint" />
        {{ roleLabel }}
      </span>
    </div>

    <div class="mono flex shrink-0 items-center gap-3 tabular-nums">
      <span class="flex items-center gap-1.5" :class="online ? 'text-muted' : 'text-warning'">
        <Wifi v-if="online" :size="11" class="text-success" />
        <WifiOff v-else :size="11" class="text-warning" />
        {{ online ? t('在线') : t('离线') }}
      </span>
      <span class="flex items-center gap-1.5">
        <EyeOff :size="11" class="text-faint" />
        {{ settings.startHidden ? t('隐藏启动') : t('显示窗口') }}
      </span>
      <span class="h-3 w-px bg-line" />
      <span class="text-faint">v{{ agent.version }}</span>
      <span class="h-3 w-px bg-line" />
      <span>{{ timezoneLabel }} {{ clockLabel }}</span>
    </div>
  </footer>
</template>
