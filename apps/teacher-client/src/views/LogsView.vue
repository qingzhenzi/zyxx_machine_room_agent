<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RefreshCw, ScrollText } from 'lucide-vue-next'

import DaemonOfflinePanel from '@/components/shell/DaemonOfflinePanel.vue'
import BrandMark from '@/components/ui/BrandMark.vue'
import EmptyState from '@/components/ui/EmptyState.vue'
import LoadingState from '@/components/ui/LoadingState.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import type { AgentLogLevel } from '@/stores/agent'
import { useAgentStore } from '@/stores/agent'

const { t } = useI18n()
const agent = useAgentStore()
const refreshing = ref(false)

const levelTone: Record<AgentLogLevel, 'info' | 'warning' | 'danger' | 'muted'> = {
  TRACE: 'muted',
  DEBUG: 'muted',
  INFO: 'info',
  WARN: 'warning',
  ERROR: 'danger',
  LOG: 'muted',
}

const countLabel = computed(() => t('共 {count} 条', { count: agent.logs.length }))

async function refresh() {
  refreshing.value = true
  await agent.refreshLogs()
  refreshing.value = false
}

let stopPolling: (() => void) | undefined

onMounted(() => {
  stopPolling = agent.startLogsPolling()
})

onUnmounted(() => {
  stopPolling?.()
})
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div class="mx-auto max-w-[880px] px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">
              {{ t('运行日志') }}
            </h1>
            <StatusBadge
              :label="agent.connected ? t('已连接 daemon') : t('未连接')"
              :tone="agent.connected ? 'success' : 'danger'"
            />
          </div>
          <p class="mt-0.5 text-[11px] text-muted">
            {{ t('来自 qe-agent 的最近日志（IPC /v1/logs），每 2 秒刷新。') }}
          </p>
        </div>
      </header>

      <section v-if="agent.connection === 'unknown'" class="panel">
        <LoadingState :label="t('正在连接 qe-agent daemon…')" compact />
      </section>

      <DaemonOfflinePanel
        v-else-if="!agent.connected"
        :error="agent.logsError ?? agent.error"
      />

      <section v-else class="panel overflow-hidden">
        <div class="panel-header">
          <div>
            <h2 class="panel-title">{{ t('日志') }}</h2>
            <p class="panel-subtitle">
              {{ t('最近 {count} 行；日志保存在 daemon 内存环形缓冲区。', { count: 200 }) }}
            </p>
          </div>
          <div class="flex items-center gap-2">
            <span class="mono text-[9px] text-faint">{{ countLabel }}</span>
            <button type="button" class="btn btn-sm" :disabled="refreshing" @click="refresh">
              <RefreshCw :size="11" :class="{ 'animate-spin-slow': refreshing }" />
              {{ t('刷新') }}
            </button>
          </div>
        </div>

        <div v-if="agent.logs.length" class="scroll-area max-h-[560px] border-t border-line">
          <div
            v-for="entry in agent.logs"
            :key="entry.id"
            class="row-hover flex items-start gap-3 border-b border-line px-4 py-1.5 last:border-b-0"
            :title="entry.raw"
          >
            <span class="mono w-[60px] shrink-0 pt-1 text-[10px] tabular-nums text-faint">
              {{ entry.time || '—' }}
            </span>
            <StatusBadge
              class="mt-0.5 shrink-0"
              :label="entry.level"
              :tone="levelTone[entry.level]"
              :dot="false"
            />
            <span class="mono min-w-0 flex-1 break-all text-[10.5px] leading-4 text-ink-soft">
              {{ entry.message }}
            </span>
          </div>
        </div>

        <div v-else class="border-t border-line">
          <EmptyState
            :icon="ScrollText"
            :title="t('暂无日志')"
            :description="t('daemon 尚未产生日志，或日志缓冲为空。')"
            :action-label="t('刷新')"
            @action="refresh"
          />
        </div>
      </section>
    </div>
  </div>
</template>
