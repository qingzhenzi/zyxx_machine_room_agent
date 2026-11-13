<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { CheckCircle2, Wifi, WifiOff } from 'lucide-vue-next'

import { useClock } from '@/composables/useClock'
import { useNetworkStatus } from '@/composables/useNetworkStatus'
import { appConfig } from '@/config/app'

const { t } = useI18n()
const { clockLabel, timezoneLabel } = useClock()
const { online } = useNetworkStatus()
</script>

<template>
  <footer
    class="flex h-[24px] select-none items-center justify-between border-t border-line bg-surface px-3 text-[10.5px] text-muted"
  >
    <div class="flex min-w-0 items-center gap-3">
      <span class="flex items-center gap-1.5">
        <CheckCircle2 :size="11" class="text-success" />
        {{ t('status.ready') }}
      </span>
      <span class="h-3 w-px bg-line" />
      <span class="flex items-center gap-1.5" :class="online ? 'text-muted' : 'text-warning'">
        <Wifi v-if="online" :size="11" class="text-success" />
        <WifiOff v-else :size="11" class="text-warning" />
        {{ online ? t('status.online') : t('status.offline') }}
      </span>
    </div>

    <div class="mono flex shrink-0 items-center gap-3 tabular-nums">
      <span class="text-faint">v{{ appConfig.version }}</span>
      <span class="h-3 w-px bg-line" />
      <span>{{ timezoneLabel }} {{ clockLabel }}</span>
    </div>
  </footer>
</template>
