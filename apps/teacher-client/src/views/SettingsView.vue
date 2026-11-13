<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { Cpu, Monitor, Moon, RefreshCw, RotateCcw, Sun } from 'lucide-vue-next'

import BrandMark from '@/components/ui/BrandMark.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import ToggleSwitch from '@/components/ui/ToggleSwitch.vue'
import { localeOptions, type AppLocale } from '@/i18n'
import { autostartEnabled, setAutostart } from '@/lib/desktop'
import { isTauri } from '@/lib/platform'
import type { ThemeMode } from '@/lib/settings'
import { useAgentStore } from '@/stores/agent'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const settings = useSettingsStore()
const agent = useAgentStore()
const ui = useUiStore()

interface ThemeOption {
  id: ThemeMode
  label: string
  description: string
  icon: Component
}

const themeOptions = computed<ThemeOption[]>(() => [
  { id: 'dark', label: t('深色'), description: t('适合长时间专注'), icon: Moon },
  { id: 'light', label: t('浅色'), description: t('高对比度的明亮界面'), icon: Sun },
  { id: 'system', label: t('跟随系统'), description: t('自动匹配系统外观'), icon: Monitor },
])

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

const intervalLabel = computed(() => {
  const intervals = agent.intervals
  if (!intervals) return '—'
  return `${intervals.fast_interval_secs}s / ${intervals.full_interval_secs}s`
})

const daemonRows = computed(() => [
  {
    label: t('连接状态'),
    value: agent.connected ? t('已连接 daemon') : t('未连接'),
  },
  { label: t('agent 角色'), value: roleLabel.value },
  { label: t('服务端地址'), value: agent.status?.mtls_endpoint ?? '—' },
  { label: t('IPC socket'), value: agent.status?.socket ?? agent.socketPath },
  { label: t('报告模式'), value: agent.status?.report_mode ?? '—' },
  { label: t('采集间隔'), value: intervalLabel.value },
  { label: t('策略来源'), value: agent.policy?.source ?? '—' },
  { label: t('daemon 版本'), value: `v${agent.version}` },
])

const refreshing = ref(false)
const autostartBusy = ref(false)

onMounted(() => {
  void agent.ensureStatus()
  void agent.refreshPolicy()

  if (!isTauri) return
  void autostartEnabled().then((enabled) => {
    settings.launchAtStartup = enabled
  })
})

async function refreshDaemon() {
  refreshing.value = true
  await agent.ensureStatus()
  await agent.refreshPolicy()
  refreshing.value = false
}

async function toggleAutostart(value: boolean) {
  settings.launchAtStartup = value
  autostartBusy.value = true
  const enabled = await setAutostart(value)
  autostartBusy.value = false

  if (enabled !== value) {
    ui.pushToast({
      kind: 'warning',
      title: t('无法修改开机自启'),
      detail: t('浏览器预览或系统权限受限；偏好已保存在本地。'),
    })
  }
}

function setLocaleOption(locale: AppLocale) {
  settings.locale = locale
}

function resetSettings() {
  settings.reset()
  ui.pushToast({
    kind: 'success',
    title: t('已恢复默认设置'),
    detail: t('主题、语言与启动偏好已重置。'),
  })
}
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div class="mx-auto max-w-[880px] px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">
            {{ t('设置') }}
          </h1>
          <p class="mt-0.5 text-[11px] text-muted">
            {{ t('客户端外观、启动方式与 daemon 运行信息。') }}
          </p>
        </div>
      </header>

      <section class="panel p-4">
        <div class="flex items-start justify-between">
          <div>
            <h2 class="text-[12px] font-semibold text-ink">{{ t('主题') }}</h2>
            <p class="mt-0.5 text-[10px] text-muted">{{ t('选择客户端的外观模式。') }}</p>
          </div>
          <span class="mono text-[9px] text-faint">{{ settings.resolvedTheme }}</span>
        </div>
        <div class="mt-3 grid grid-cols-3 gap-2.5">
          <button
            v-for="option in themeOptions"
            :key="option.id"
            type="button"
            class="flex flex-col rounded-md border p-2.5 text-left transition-colors"
            :class="
              settings.theme === option.id
                ? 'border-accent bg-accent-soft'
                : 'border-line bg-surface-2 hover:border-line-strong'
            "
            :aria-pressed="settings.theme === option.id"
            @click="settings.setTheme(option.id)"
          >
            <span class="flex items-center gap-1.5">
              <component :is="option.icon" :size="12" class="text-muted" />
              <span class="text-[10.5px] font-medium text-ink">{{ option.label }}</span>
            </span>
            <span class="mt-0.5 text-[9px] text-faint">{{ option.description }}</span>
          </button>
        </div>
      </section>

      <section class="panel mt-3 overflow-hidden">
        <div class="panel-header">
          <div>
            <h2 class="panel-title">{{ t('启动与运行') }}</h2>
            <p class="panel-subtitle">{{ t('托盘常驻；窗口默认隐藏启动。') }}</p>
          </div>
          <Cpu :size="13" class="text-faint" />
        </div>

        <div class="divide-y divide-line border-t border-line">
          <div class="flex items-center justify-between gap-4 px-4 py-3">
            <div>
              <p class="text-[11px] font-medium text-ink">{{ t('开机自启') }}</p>
              <p class="mt-0.5 text-[10px] text-muted">{{ t('开机后自动在托盘运行。') }}</p>
            </div>
            <ToggleSwitch
              :model-value="settings.launchAtStartup"
              :disabled="autostartBusy"
              :label="t('开机自启')"
              @update:model-value="toggleAutostart"
            />
          </div>

          <div class="flex items-center justify-between gap-4 px-4 py-3">
            <div>
              <p class="text-[11px] font-medium text-ink">{{ t('隐藏启动') }}</p>
              <p class="mt-0.5 text-[10px] text-muted">
                {{ t('启动时只显示托盘图标，不弹出窗口。') }}
              </p>
              <p class="mt-1 text-[9px] text-faint">
                {{ t('窗口默认隐藏由 tauri.conf.json 固定；daemon 由 systemd 独立常驻。') }}
              </p>
            </div>
            <ToggleSwitch v-model="settings.startHidden" :label="t('隐藏启动')" />
          </div>
        </div>
      </section>

      <section class="panel mt-3 overflow-hidden">
        <div class="panel-header">
          <div>
            <h2 class="panel-title">{{ t('daemon 信息') }}</h2>
            <p class="panel-subtitle">{{ t('只读：来自 qe-agent /v1/status 与 /v1/policy。') }}</p>
          </div>
          <div class="flex items-center gap-2">
            <StatusBadge
              :label="agent.connected ? t('已连接 daemon') : t('未连接')"
              :tone="agent.connected ? 'success' : 'danger'"
            />
            <button type="button" class="btn btn-sm" :disabled="refreshing" @click="refreshDaemon">
              <RefreshCw :size="11" :class="{ 'animate-spin-slow': refreshing }" />
              {{ t('刷新状态') }}
            </button>
          </div>
        </div>
        <dl class="divide-y divide-line border-t border-line">
          <div
            v-for="row in daemonRows"
            :key="row.label"
            class="flex items-center justify-between gap-4 px-4 py-2.5"
          >
            <dt class="shrink-0 text-[10.5px] text-muted">{{ row.label }}</dt>
            <dd class="mono max-w-[64%] truncate text-right text-[10.5px] text-ink-soft">
              {{ row.value }}
            </dd>
          </div>
        </dl>
        <div class="border-t border-line px-4 py-2">
          <p class="text-[9px] leading-4 text-faint">
            {{ t('角色、服务端与 socket 由 qe-agent 决定，GUI 只读；写策略仍走管理端。') }}
          </p>
        </div>
      </section>

      <section class="panel mt-3 flex items-center justify-between gap-4 p-4">
        <div>
          <h2 class="text-[12px] font-semibold text-ink">{{ t('界面语言') }}</h2>
          <p class="mt-0.5 text-[10px] text-muted">{{ t('选择客户端界面语言，切换后立即生效。') }}</p>
        </div>
        <div class="flex shrink-0 items-center gap-1.5">
          <button
            v-for="option in localeOptions"
            :key="option.id"
            type="button"
            class="rounded border px-2.5 py-1 text-[10px] transition-colors"
            :class="
              settings.locale === option.id
                ? 'border-accent-line bg-accent-soft font-medium text-accent'
                : 'border-line bg-surface-2 text-muted hover:text-ink'
            "
            :aria-pressed="settings.locale === option.id"
            @click="setLocaleOption(option.id)"
          >
            {{ option.label }}
          </button>
        </div>
      </section>

      <section class="panel mt-3 flex items-start gap-3 p-4">
        <BrandMark :size="34" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h2 class="text-[13px] font-semibold text-ink">QEStudio Agent</h2>
            <span
              class="rounded border border-accent-line bg-accent-soft px-1.5 py-px text-[9px] font-medium text-accent"
            >
              P3 IPC
            </span>
          </div>
          <p class="mono mt-1 text-[10px] text-faint">
            version {{ agent.version }} · identifier org.qestudio.agent
          </p>
          <p class="mt-1 text-[10px] text-muted">
            {{ t('通过 qe-agent 本机 IPC 读取状态与日志；证书/私钥保留在 daemon 数据目录。') }}
          </p>
        </div>
        <button type="button" class="btn btn-sm shrink-0" @click="resetSettings">
          <RotateCcw :size="11" />
          {{ t('恢复默认设置') }}
        </button>
      </section>
    </div>
  </div>
</template>
