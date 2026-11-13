<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Copy, PlugZap, RefreshCw, Sparkles } from 'lucide-vue-next'

import { useAgentStore } from '@/stores/agent'
import { useUiStore } from '@/stores/ui'
import { useRouter } from 'vue-router'

const props = defineProps<{ error?: string | null }>()

const { t } = useI18n()
const agent = useAgentStore()
const ui = useUiStore()
const router = useRouter()
const retrying = ref(false)

const message = computed(() => props.error || agent.error || t('daemon 未运行。'))

const commands = computed(() => [
  {
    label: t('启动 daemon（开发）'),
    command: `qe-agent run --role both --ipc-socket ${agent.socketPath}`,
  },
  {
    label: t('或只启动 IPC 调试'),
    command: `qe-agent serve --ipc-socket ${agent.socketPath}`,
  },
  {
    label: t('查看 systemd 服务'),
    command: 'systemctl status qe-agent',
  },
  {
    label: t('开发时覆盖 socket 路径'),
    command: `QE_AGENT_IPC_SOCKET=${agent.socketPath} pnpm tauri dev`,
  },
])

async function retry() {
  retrying.value = true
  const connected = await agent.ensureStatus()
  retrying.value = false
  ui.pushToast({
    kind: connected ? 'success' : 'warning',
    title: connected ? t('已连接 daemon') : t('仍未连接 daemon'),
    detail: connected ? agent.socketPath : message.value,
  })
}

async function copy(command: string) {
  try {
    await navigator.clipboard.writeText(command)
    ui.pushToast({ kind: 'success', title: t('已复制命令') })
  } catch {
    ui.pushToast({
      kind: 'warning',
      title: t('复制失败'),
      detail: t('请手动选择命令复制。'),
    })
  }
}
</script>

<template>
  <section class="panel overflow-hidden">
    <div class="flex items-start gap-3 border-b border-line bg-danger-soft px-4 py-3">
      <span
        class="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-full border border-danger/30 bg-surface text-danger"
      >
        <PlugZap :size="15" />
      </span>
      <div class="min-w-0 flex-1">
        <div class="flex flex-wrap items-center gap-2">
          <h2 class="text-[12.5px] font-semibold text-ink">{{ t('未连接 qe-agent daemon') }}</h2>
          <div class="ml-auto flex items-center gap-1.5">
            <button
              type="button"
              class="btn btn-sm"
              @click="router.push('/setup')"
            >
              <Sparkles :size="11" />
              {{ t('初始化向导') }}
            </button>
            <button
              type="button"
              class="btn btn-sm"
              :disabled="retrying"
              @click="retry"
            >
              <RefreshCw :size="11" :class="{ 'animate-spin-slow': retrying }" />
              {{ t('重试连接') }}
            </button>
          </div>
        </div>
        <p class="mt-1 text-[10.5px] leading-4 text-muted">
          {{ t('GUI 通过本机 Unix socket 读取 daemon 状态；请先启动 daemon，或检查 socket 路径与权限。') }}
        </p>
        <p class="mono mt-1.5 break-all text-[10px] leading-4 text-danger">{{ message }}</p>
      </div>
    </div>

    <div class="border-b border-line px-4 py-2.5">
      <div class="flex items-center justify-between gap-3">
        <span class="section-label">{{ t('IPC socket') }}</span>
        <span class="mono truncate text-[10.5px] text-ink-soft" :title="agent.socketPath">
          {{ agent.socketPath }}
        </span>
      </div>
      <p class="mt-1 text-[9.5px] text-faint">
        {{ t('socket 路径可由 QE_AGENT_IPC_SOCKET 覆盖（默认 /run/qe-agent/agent.sock）。') }}
      </p>
    </div>

    <div class="px-4 py-3">
      <h3 class="text-[11px] font-medium text-ink">{{ t('启动引导') }}</h3>
      <ul class="mt-2 space-y-1.5">
        <li
          v-for="item in commands"
          :key="item.command"
          class="flex items-center gap-2 rounded border border-line bg-canvas px-2.5 py-1.5"
        >
          <span class="w-[112px] shrink-0 text-[9.5px] text-faint">{{ item.label }}</span>
          <code class="mono min-w-0 flex-1 truncate text-[10px] text-ink-soft" :title="item.command">
            {{ item.command }}
          </code>
          <button
            type="button"
            class="flex h-5 w-5 shrink-0 items-center justify-center rounded text-faint transition-colors hover:bg-surface-2 hover:text-ink"
            :aria-label="t('复制')"
            :title="t('复制')"
            @click="copy(item.command)"
          >
            <Copy :size="11" />
          </button>
        </li>
      </ul>
    </div>
  </section>
</template>
