<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import {
  ArrowRight,
  CheckCircle2,
  Download,
  Play,
  RefreshCw,
  RotateCw,
  Terminal,
  XCircle,
} from 'lucide-vue-next'

import { useAgentStore } from '@/stores/agent'
import { useUiStore } from '@/stores/ui'
import {
  installDaemon,
  manualInstallCommand,
  probeSetup,
  serviceAction,
  type SetupProbe,
  type SetupResult,
} from '@/lib/setup'

const router = useRouter()
const agent = useAgentStore()
const ui = useUiStore()

const probe = ref<SetupProbe | null>(null)
const loading = ref(false)
const busy = ref<'install' | 'start' | 'restart' | null>(null)
const error = ref<string | null>(null)
const agentBin = ref('/usr/local/bin/qe-agent')
const result = ref<SetupResult | null>(null)

const checks = computed(() => {
  const p = probe.value
  if (!p) return []
  return [
    { label: 'Linux + systemd', ok: p.platform === 'linux' && p.systemd, detail: p.platform },
    { label: 'pkexec（PolicyKit）', ok: Boolean(p.pkexec), detail: p.pkexec ?? '未安装 polkit' },
    { label: '安装脚本资源', ok: Boolean(p.setup_script), detail: p.setup_script ?? '缺失' },
    { label: 'qe-agent 二进制', ok: Boolean(p.agent_bin), detail: p.agent_bin ?? '未找到，可手工指定路径' },
    { label: 'systemd 单元', ok: p.unit_installed, detail: p.unit_installed ? '已安装' : '未安装' },
    { label: '服务运行中', ok: p.service_active, detail: p.service_active ? 'active' : 'inactive' },
    { label: 'qe-agent 组', ok: p.group_exists, detail: p.group_exists ? '已创建' : '未创建' },
    {
      label: '当前用户在 qe-agent 组',
      ok: p.user_in_group,
      detail: p.user_in_group ? p.current_user : `${p.current_user}（安装时会加入，需注销重登）`,
    },
  ]
})

const canInstall = computed(
  () =>
    Boolean(probe.value?.systemd && probe.value?.pkexec && probe.value?.setup_script) &&
    agentBin.value.trim().length > 0,
)

const manualCommand = computed(() => manualInstallCommand(probe.value, agentBin.value.trim()))

async function refresh() {
  loading.value = true
  error.value = null
  try {
    probe.value = await probeSetup()
    if (probe.value.agent_bin) agentBin.value = probe.value.agent_bin
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    loading.value = false
  }
}

async function install() {
  busy.value = 'install'
  error.value = null
  result.value = null
  try {
    result.value = await installDaemon(agentBin.value.trim(), probe.value?.current_user, true)
    if (result.value.ok) {
      ui.pushToast({ kind: 'success', title: 'daemon 安装完成' })
      await refresh()
      await agent.ensureStatus()
    } else {
      ui.pushToast({ kind: 'warning', title: '安装未完成', detail: result.value.stderr || result.value.stdout })
    }
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    busy.value = null
  }
}

async function service(action: 'start' | 'restart') {
  busy.value = action
  error.value = null
  result.value = null
  try {
    result.value = await serviceAction(action)
    if (result.value.ok) {
      ui.pushToast({ kind: 'success', title: action === 'start' ? '服务已启动' : '服务已重启' })
      await refresh()
      await agent.ensureStatus()
    }
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    busy.value = null
  }
}

onMounted(() => {
  void refresh()
})
</script>

<template>
  <div class="mx-auto flex w-full max-w-[980px] flex-col gap-4">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <h1 class="text-[18px] font-semibold text-ink">安装与初始化</h1>
        <p class="mt-1 text-[11px] leading-5 text-muted">
          在桌面端内检测本机环境、安装 qe-agent daemon、启动 systemd 服务并加入 qe-agent 组；
          安装需要 PolicyKit 授权（pkexec），不会保存 root 密码。
        </p>
      </div>
      <button type="button" class="btn btn-sm" :disabled="loading" @click="refresh">
        <RefreshCw :size="12" :class="{ 'animate-spin-slow': loading }" />
        重新检测
      </button>
    </header>

    <section class="panel px-4 py-3">
      <div class="flex items-center justify-between gap-2">
        <h2 class="section-label">环境检查</h2>
        <span class="mono text-[10px] text-faint">{{ probe?.socket_path ?? '/run/qe-agent/agent.sock' }}</span>
      </div>
      <p v-if="error" class="mono mt-2 rounded border border-danger/30 bg-danger-soft px-2.5 py-2 text-[10.5px] text-danger">
        {{ error }}
      </p>
      <div v-if="probe" class="mt-2 grid grid-cols-1 gap-1.5 md:grid-cols-2">
        <div
          v-for="item in checks"
          :key="item.label"
          class="flex items-center gap-2 rounded border border-line bg-canvas px-2.5 py-1.5"
        >
          <CheckCircle2 v-if="item.ok" :size="13" class="shrink-0 text-emerald-500" />
          <XCircle v-else :size="13" class="shrink-0 text-amber-500" />
          <span class="shrink-0 text-[10.5px] text-ink">{{ item.label }}</span>
          <span class="mono ml-auto truncate text-[9.5px] text-faint" :title="item.detail ?? ''">
            {{ item.detail }}
          </span>
        </div>
      </div>
      <div v-else class="mt-3 text-[11px] text-muted">{{ loading ? '检测中…' : '无法读取环境信息' }}</div>
    </section>

    <section class="panel px-4 py-3">
      <h2 class="section-label">安装并启动</h2>
      <label class="mt-2 block text-[10.5px] text-muted">
        qe-agent 二进制路径（构建产物或已安装路径）
        <input
          v-model="agentBin"
          class="mono mt-1 w-full rounded border border-line bg-canvas px-2.5 py-1.5 text-[11px] text-ink outline-none focus:border-accent"
          placeholder="/usr/local/bin/qe-agent"
        />
      </label>
      <div class="mt-3 flex flex-wrap items-center gap-2">
        <button type="button" class="btn btn-sm btn-primary" :disabled="!canInstall || busy !== null" @click="install">
          <Download :size="12" class="mr-1" />
          {{ busy === 'install' ? '安装中…' : '安装并启动 daemon' }}
        </button>
        <button type="button" class="btn btn-sm" :disabled="!probe?.unit_installed || busy !== null" @click="service('start')">
          <Play :size="12" class="mr-1" />
          启动服务
        </button>
        <button type="button" class="btn btn-sm" :disabled="!probe?.unit_installed || busy !== null" @click="service('restart')">
          <RotateCw :size="12" class="mr-1" />
          重启服务
        </button>
        <button
          type="button"
          class="btn btn-sm ml-auto"
          :disabled="!probe?.service_active"
          @click="router.push('/enroll')"
        >
          下一步：注册设备
          <ArrowRight :size="12" class="ml-1" />
        </button>
      </div>
      <p v-if="!probe?.pkexec" class="mt-2 text-[10px] text-amber-600">
        未找到 pkexec：请安装 polkit，或用下方 sudo 命令手动安装。
      </p>
      <p v-if="!probe?.systemd" class="mt-1 text-[10px] text-amber-600">
        当前环境不是 systemd Linux，自动安装不可用。
      </p>
    </section>

    <section class="panel px-4 py-3">
      <div class="flex items-center gap-2">
        <Terminal :size="13" class="text-muted" />
        <h2 class="section-label">手动安装命令（等价）</h2>
      </div>
      <pre class="mono mt-2 overflow-x-auto rounded border border-line bg-canvas px-3 py-2 text-[10px] leading-5 text-ink-soft">{{ manualCommand }}</pre>
      <p class="mt-1.5 text-[9.5px] text-faint">
        安装脚本位于 agent-client 资源目录；也可用仓库内
        <span class="mono">apps/agent-client/scripts/install-agent-client.sh</span>
        只安装 GUI。
      </p>
    </section>

    <section v-if="result" class="panel px-4 py-3">
      <h2 class="section-label">执行输出</h2>
      <p
        class="mono mt-2 rounded border px-2.5 py-2 text-[10px] leading-5"
        :class="result.ok ? 'border-emerald-500/30 bg-emerald-500/5 text-emerald-600' : 'border-danger/30 bg-danger-soft text-danger'"
      >
        {{ result.ok ? '✔ 成功' : '✖ 失败' }} · {{ result.command }}
      </p>
      <pre v-if="result.stdout" class="mono mt-2 max-h-[220px] overflow-auto rounded bg-canvas px-3 py-2 text-[10px] leading-5 text-ink-soft">{{ result.stdout }}</pre>
      <pre v-if="result.stderr" class="mono mt-2 max-h-[220px] overflow-auto rounded bg-danger-soft px-3 py-2 text-[10px] leading-5 text-danger">{{ result.stderr }}</pre>
    </section>
  </div>
</template>
