<script setup lang="ts">
import { computed, onMounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Activity,
  Clock,
  Cpu,
  FolderOpen,
  Hash,
  Info,
  KeyRound,
  Link2,
  Power,
  ScrollText,
  Server,
  ShieldCheck,
  Tag,
} from 'lucide-vue-next'
import { RouterLink } from 'vue-router'

import DaemonOfflinePanel from '@/components/shell/DaemonOfflinePanel.vue'
import BrandMark from '@/components/ui/BrandMark.vue'
import LoadingState from '@/components/ui/LoadingState.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { useAgentStore } from '@/stores/agent'
import { useSettingsStore } from '@/stores/settings'

const { t } = useI18n()
const agent = useAgentStore()
const settings = useSettingsStore()

const statusLabel = computed(() => {
  if (!agent.connected) return t('未连接')
  return agent.registered ? t('已注册') : t('未注册')
})

const statusTone = computed(() => {
  if (!agent.connected) return 'danger'
  return agent.registered ? 'success' : 'warning'
})

const launchLabel = computed(() =>
  settings.startHidden ? t('隐藏启动 · 仅托盘') : t('显示窗口'),
)

const roleLabel = computed(() => {
  if (agent.roles.length === 0) return t('未运行角色（serve 仅 IPC）')
  return agent.roles
    .map((role) =>
      role === 'node'
        ? t('节点（node）')
        : role === 'machine'
          ? t('机器（machine）')
          : role,
    )
    .join(' + ')
})

const rows = computed(() => {
  const status = agent.status
  return [
    { label: t('注册状态'), value: statusLabel.value, icon: ShieldCheck },
    { label: t('服务端地址'), value: status?.mtls_endpoint ?? t('未配置'), icon: Server },
    { label: t('agent 角色'), value: roleLabel.value, icon: Cpu },
    { label: t('报告模式'), value: status?.report_mode ?? '—', icon: Activity },
    { label: t('设备槽位'), value: status?.device_slot ?? '—', icon: Hash },
    { label: t('设备序列号'), value: status?.serial ?? '—', icon: KeyRound },
    { label: t('daemon 版本'), value: status ? `v${status.version}` : '—', icon: Tag },
    {
      label: t('运行时长'),
      value: status ? formatUptime(status.uptime_secs) : '—',
      icon: Clock,
    },
    { label: t('IPC socket'), value: status?.socket ?? agent.socketPath, icon: Power },
    { label: t('数据目录'), value: status?.data_dir ?? '—', icon: FolderOpen },
    { label: t('启动方式'), value: launchLabel.value, icon: Power },
  ]
})

const intervalLabel = computed(() => {
  const intervals = agent.intervals
  if (!intervals) return '—'
  return `${intervals.fast_interval_secs}s fast · ${intervals.full_interval_secs}s full`
})

const updatedLabel = computed(() => {
  if (!agent.lastUpdated) return '—'
  return new Date(agent.lastUpdated).toLocaleTimeString(undefined, {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
})

function formatUptime(seconds: number): string {
  const total = Math.max(0, Math.round(seconds))
  if (total < 60) return `${total}s`
  const minutes = Math.floor(total / 60)
  if (minutes < 60) return `${minutes}m ${total % 60}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

onMounted(() => {
  void agent.ensureStatus()
  void agent.refreshPolicy()
})

watch(
  () => agent.connected,
  (connected) => {
    if (connected) void agent.refreshPolicy()
  },
)
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div class="mx-auto max-w-[880px] px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">
              {{ t('机器 Agent 状态') }}
            </h1>
            <StatusBadge :label="statusLabel" :tone="statusTone" />
          </div>
          <p class="mt-0.5 text-[11px] text-muted">
            {{
              agent.connected
                ? t('通过 qe-agent 本机 IPC 读取真实状态。')
                : t('未连接 daemon：以下为启动引导与 socket 信息。')
            }}
          </p>
        </div>
      </header>

      <section v-if="agent.connection === 'unknown'" class="panel">
        <LoadingState :label="t('正在连接 qe-agent daemon…')" compact />
      </section>

      <DaemonOfflinePanel
        v-else-if="!agent.connected"
        :error="agent.error"
      />

      <template v-else>
        <section class="panel overflow-hidden">
          <div class="panel-header">
            <div>
              <h2 class="panel-title">{{ t('daemon 状态') }}</h2>
              <p class="panel-subtitle">{{ t('数据来自 qe-agent /v1/status。') }}</p>
            </div>
            <span class="mono text-[9px] text-faint">
              {{ t('最近更新') }} {{ updatedLabel }}
            </span>
          </div>
          <dl class="divide-y divide-line border-t border-line">
            <div
              v-for="row in rows"
              :key="row.label"
              class="flex items-center justify-between gap-4 px-4 py-2.5"
            >
              <dt class="flex shrink-0 items-center gap-2 text-[10.5px] text-muted">
                <component :is="row.icon" :size="12" class="text-faint" />
                {{ row.label }}
              </dt>
              <dd class="mono max-w-[62%] truncate text-right text-[10.5px] text-ink-soft">
                {{ row.value }}
              </dd>
            </div>
          </dl>
          <div class="flex flex-wrap items-center gap-2 border-t border-line px-4 py-3">
            <RouterLink to="/enroll" class="btn btn-primary">
              <Link2 :size="12" />
              {{ t('注册到服务端') }}
            </RouterLink>
            <RouterLink to="/logs" class="btn">
              <ScrollText :size="12" />
              {{ t('查看日志') }}
            </RouterLink>
            <span class="ml-auto text-[9.5px] text-faint">
              {{ t('注册与日志通过 qe-agent 本机 IPC 完成。') }}
            </span>
          </div>
        </section>

        <section class="panel mt-3 p-4">
          <div class="flex items-start justify-between gap-3">
            <div>
              <h2 class="text-[12px] font-semibold text-ink">{{ t('采集策略') }}</h2>
              <p class="mt-0.5 text-[10px] text-muted">
                {{ t('本地解析值（远端策略回读留待 P4）。') }}
              </p>
            </div>
            <span class="mono text-[9px] text-faint">
              {{ agent.policy?.source ?? 'local' }}
            </span>
          </div>
          <div class="mt-2 grid grid-cols-3 gap-2.5">
            <div class="rounded border border-line bg-surface-2 px-3 py-2">
              <p class="text-[9px] text-faint">{{ t('采集间隔') }}</p>
              <p class="mono mt-0.5 text-[11px] text-ink-soft">{{ intervalLabel }}</p>
            </div>
            <div class="rounded border border-line bg-surface-2 px-3 py-2">
              <p class="text-[9px] text-faint">{{ t('策略来源') }}</p>
              <p class="mono mt-0.5 text-[11px] text-ink-soft">
                {{ agent.policy?.source ?? 'local' }}
              </p>
            </div>
            <div class="rounded border border-line bg-surface-2 px-3 py-2">
              <p class="text-[9px] text-faint">{{ t('配置修订') }}</p>
              <p class="mono mt-0.5 text-[11px] text-ink-soft">
                {{ agent.policy?.revision ?? '—' }}
              </p>
            </div>
          </div>
        </section>
      </template>

      <section class="panel mt-3 p-4">
        <h2 class="flex items-center gap-1.5 text-[12px] font-semibold text-ink">
          <Info :size="12" class="text-muted" />
          {{ t('P3 范围') }}
        </h2>
        <ul class="mt-2 space-y-1.5 text-[10.5px] leading-4 text-muted">
          <li>• {{ t('GUI 只读取 daemon 状态与日志，不接触证书或私钥；写策略仍走管理端。') }}</li>
          <li>• {{ t('状态每 2 秒轮询 /v1/status；daemon 未运行时显示未连接与启动引导。') }}</li>
          <li>• {{ t('窗口默认隐藏与托盘行为保持不变；关闭窗口仅隐藏回托盘。') }}</li>
        </ul>
      </section>
    </div>
  </div>
</template>
