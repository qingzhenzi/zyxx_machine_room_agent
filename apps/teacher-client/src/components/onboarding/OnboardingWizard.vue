<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import {
  AlertTriangle,
  CheckCircle2,
  Cpu,
  Download,
  Link2,
  Loader2,
  RefreshCw,
  ShieldCheck,
} from 'lucide-vue-next'

import { isTauri } from '@/lib/platform'
import {
  installDaemon,
  probeSetup,
  manualInstallCommand,
  enrollDaemon,
  type SetupProbe,
  type SetupResult,
} from '@/lib/setup'
import { useAgentStore } from '@/stores/agent'

const emit = defineEmits<{ done: [] }>()

const agent = useAgentStore()
const phase = ref<'checking' | 'install' | 'enroll' | 'ready' | 'error'>('checking')
const progress = ref(0)
const probe = ref<SetupProbe | null>(null)
const error = ref<string | null>(null)
const AGENT_BIN_KEY = 'qe-agent-client.agent-bin.v1'
const agentBin = ref(
  (() => {
    try {
      return localStorage.getItem(AGENT_BIN_KEY) || '/usr/local/bin/qe-agent'
    } catch {
      return '/usr/local/bin/qe-agent'
    }
  })(),
)
const AGENT_CONFIG_KEY = 'qe-agent-client.agent-config.v1'
const savedConfig = (() => {
  try {
    const raw = localStorage.getItem(AGENT_CONFIG_KEY)
    return raw ? (JSON.parse(raw) as Record<string, unknown>) : {}
  } catch {
    return {}
  }
})()
const allowPoweroff = ref(savedConfig.allowPoweroff !== false)
const fastInterval = ref(String(savedConfig.fastIntervalSecs ?? 10))
const fullInterval = ref(String(savedConfig.fullIntervalSecs ?? 300))
const liveIntervalMs = ref(String(savedConfig.liveIntervalMs ?? 500))
const livePush = ref(savedConfig.livePush === true)
const busy = ref<'install' | 'enroll' | null>(null)
const result = ref<SetupResult | null>(null)
const enrollLink = ref('')
const enrollMessage = ref<string | null>(null)
let timer: number | undefined
let monitor: number | undefined
const daemonConnected = computed(() => agent.connected)

const installReady = computed(() => {
  const fast = Number(fastInterval.value)
  const full = Number(fullInterval.value)
  return (
    Boolean(probe.value?.systemd && probe.value?.pkexec) &&
    agentBin.value.trim().length > 0 &&
    Number.isInteger(fast) &&
    fast >= 1 &&
    fast <= 60 &&
    Number.isInteger(full) &&
    full >= 60 &&
    full <= 3600 &&
    full >= fast
  )
})
const manualCommand = computed(() => manualInstallCommand(probe.value, agentBin.value.trim()))

const checks = computed(() => {
  const p = probe.value
  if (!p) return []
  return [
    { label: 'Linux systemd', ok: p.platform === 'linux' && p.systemd },
    { label: 'PolicyKit (pkexec)', ok: Boolean(p.pkexec) },
    { label: 'qe-agent 已安装', ok: Boolean(p.agent_bin) },
    { label: 'systemd 服务运行', ok: p.service_active },
    { label: 'qe-agent 组权限', ok: p.group_exists && p.user_in_group },
  ]
})

function tickProgress(target: number, step = 6) {
  timer = window.setInterval(() => {
    if (progress.value < target) {
      progress.value = Math.min(target, progress.value + step)
    } else if (timer !== undefined) {
      window.clearInterval(timer)
      timer = undefined
    }
  }, 60)
}

async function runProbe() {
  phase.value = 'checking'
  error.value = null
  progress.value = 8
  tickProgress(92, 5)
  if (isTauri) {
    try {
      await invoke('agent_show_main_window')
    } catch {
      // 旧版本/浏览器预览忽略。
    }
  }
  try {
    const started = Date.now()
    probe.value = await probeSetup()
    if (probe.value.agent_bin) agentBin.value = probe.value.agent_bin
    // 让进度动画至少播放 ~1.4s，避免一闪而过。
    const elapsed = Date.now() - started
    if (elapsed < 1400) await new Promise((resolve) => setTimeout(resolve, 1400 - elapsed))
    progress.value = 100
    if (timer !== undefined) {
      window.clearInterval(timer)
      timer = undefined
    }
    await decide()
  } catch (cause) {
    if (timer !== undefined) {
      window.clearInterval(timer)
      timer = undefined
    }
    progress.value = 100
    error.value = cause instanceof Error ? cause.message : String(cause)
    phase.value = 'error'
  }
}

async function decide() {
  const p = probe.value
  if (!p) {
    phase.value = 'error'
    return
  }
  if (!p.systemd) {
    phase.value = 'install'
    return
  }
  if (!p.service_active) {
    phase.value = 'install'
    return
  }
  const connected = await agent.ensureStatus()
  if (!connected) {
    phase.value = 'install'
    return
  }
  phase.value = agent.registered ? 'ready' : 'enroll'
  if (phase.value === 'ready') {
    setTimeout(() => emit('done'), 900)
  }
}

async function install() {
  busy.value = 'install'
  result.value = null
  error.value = null
  try {
    result.value = await installDaemon(agentBin.value.trim(), probe.value?.current_user, true, {
      allowPoweroff: allowPoweroff.value,
      fastIntervalSecs: Number(fastInterval.value),
      fullIntervalSecs: Number(fullInterval.value),
      liveIntervalMs: Number(liveIntervalMs.value),
      livePush: livePush.value,
    })
    if (result.value.ok) {
      try {
        localStorage.setItem(AGENT_BIN_KEY, agentBin.value.trim())
        localStorage.setItem(
          AGENT_CONFIG_KEY,
          JSON.stringify({
            allowPoweroff: allowPoweroff.value,
            fastIntervalSecs: Number(fastInterval.value),
            fullIntervalSecs: Number(fullInterval.value),
            liveIntervalMs: Number(liveIntervalMs.value),
            livePush: livePush.value,
          }),
        )
      } catch {
        // ignore
      }
    }
    if (!result.value.ok) {
      error.value = result.value.stderr || result.value.stdout || '安装失败'
      return
    }
    await new Promise((resolve) => setTimeout(resolve, 700))
    probe.value = await probeSetup()
    const connected = await agent.ensureStatus()
    if (connected && agent.registered) {
      phase.value = 'ready'
      setTimeout(() => emit('done'), 900)
      return
    }
    // 未注册时 daemon 按设计不启动（否则会缺凭证 crash-loop）；
    // 安装完成后直接进入页面内注册步骤，注册 helper 会写 env 并启动服务。
    error.value = null
    phase.value = 'enroll'
    enrollMessage.value = 'daemon 已安装，请粘贴管理端注册链接完成初始化；注册成功后会自动启动 daemon。'
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    busy.value = null
  }
}

async function enroll() {
  if (!enrollLink.value.trim()) {
    enrollMessage.value = '请粘贴管理端生成的注册链接'
    return
  }
  busy.value = 'enroll'
  enrollMessage.value = null
  try {
    // daemon 在注册前不能运行（缺凭证会崩溃重启），因此 enrollment 走 root helper：
    // stop 服务 → qe-agent cli enroll → 用 meta 写 /etc/qe-agent.env → 启动服务。
    const reply = await enrollDaemon(enrollLink.value.trim())
    if (!reply.ok) {
      enrollMessage.value = reply.stderr || reply.stdout || '注册失败'
      return
    }
    enrollMessage.value = '注册成功，正在启动 daemon…'
    await new Promise((resolve) => setTimeout(resolve, 1200))
    probe.value = await probeSetup()
    await agent.ensureStatus()
    if (agent.registered) {
      enrollMessage.value = '注册成功，正在进入状态页…'
      setTimeout(() => emit('done'), 900)
    } else {
      enrollMessage.value = '注册已完成，但 daemon 状态未就绪；点「重新检测」或稍后重试'
    }
  } finally {
    busy.value = null
  }
}

onMounted(() => {
  void runProbe()
  // 向导可见期间也持续探测：systemd 服务/连接断开时立刻更新状态，
  // 不再出现“显示 daemon 已运行、实际 socket 不存在”的过期画面。
  monitor = window.setInterval(async () => {
    if (phase.value === 'checking') return
    try {
      probe.value = await probeSetup()
    } catch {
      // 保留上一次探测结果。
    }
    const connected = await agent.ensureStatus()
    if (phase.value === 'ready' && !connected) {
      phase.value = 'install'
      error.value = 'daemon 已断开（服务可能退出/crash-loop）；请重新安装或查看 journalctl -u qe-agent -n 50。'
    }
    // 未注册阶段 daemon 本来就不应运行，不在这里改注册提示；注册 helper 会启动它。
  }, 2000)
})
onUnmounted(() => {
  if (timer !== undefined) window.clearInterval(timer)
  if (monitor !== undefined) window.clearInterval(monitor)
})
</script>

<template>
  <div class="fixed inset-0 z-[80] flex items-center justify-center bg-[#0b0e12]/95 px-4 backdrop-blur-sm">
    <section class="w-full max-w-[720px] overflow-hidden rounded-xl border border-line bg-surface shadow-2xl">
      <div class="flex items-center gap-3 border-b border-line px-5 py-4">
        <span class="flex h-10 w-10 items-center justify-center rounded-lg bg-accent-soft text-accent">
          <ShieldCheck :size="20" />
        </span>
        <div class="min-w-0">
          <h1 class="text-[15px] font-semibold text-ink">QEStudio Agent 初始化</h1>
          <p class="mt-0.5 text-[10.5px] text-muted">首次启动检查设备环境，自动完成 daemon 安装与注册</p>
        </div>
      </div>

      <div class="px-5 py-4">
        <!-- 进度/步骤 -->
        <div class="flex items-center gap-2">
          <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-canvas">
            <div
              class="h-full rounded-full bg-accent transition-all duration-300 ease-out"
              :style="{ width: `${progress}%` }"
            />
          </div>
          <span class="mono w-10 text-right text-[10px] text-faint">{{ progress }}%</span>
        </div>

        <ul class="mt-4 grid grid-cols-1 gap-1.5 sm:grid-cols-2">
          <li
            v-for="item in checks"
            :key="item.label"
            class="flex items-center gap-2 rounded border border-line bg-canvas px-2.5 py-1.5"
          >
            <Loader2 v-if="phase === 'checking'" :size="13" class="animate-spin text-accent" />
            <CheckCircle2 v-else-if="item.ok" :size="13" class="text-emerald-500" />
            <AlertTriangle v-else :size="13" class="text-amber-500" />
            <span class="text-[10.5px] text-ink">{{ item.label }}</span>
          </li>
          <li
            v-if="!checks.length && phase === 'checking'"
            class="flex items-center gap-2 rounded border border-line bg-canvas px-2.5 py-1.5 text-[10.5px] text-muted"
          >
            <Loader2 :size="13" class="animate-spin text-accent" />
            正在检测本机环境…
          </li>
        </ul>

        <!-- 检查完成：已就绪 -->
        <div v-if="phase === 'ready'" class="mt-5 flex items-center gap-3 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-3 py-3">
          <CheckCircle2 :size="22" class="text-emerald-500" />
          <div>
            <p class="text-[12px] font-medium text-ink">初始化完成</p>
            <p class="mt-0.5 text-[10px] text-muted">设备已注册，正在进入状态页…</p>
          </div>
        </div>

        <!-- 需要安装 -->
        <div v-else-if="phase === 'install'" class="mt-5">
          <div class="rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2.5">
            <p class="flex items-center gap-2 text-[11.5px] font-medium text-ink">
              <Cpu :size="14" class="text-amber-500" />
              检测到 daemon 尚未安装或未运行
            </p>
            <p class="mt-1 text-[10px] leading-4 text-muted">
              将通过 PolicyKit（pkexec）安装 qe-agent、systemd 服务并启动；不会保存 root 密码。
            </p>
          </div>

          <label class="mt-3 block text-[10px] text-muted">
            qe-agent 二进制路径
            <input
              v-model="agentBin"
              class="mono mt-1 w-full rounded border border-line bg-canvas px-2.5 py-1.5 text-[11px] text-ink outline-none focus:border-accent"
              placeholder="/usr/local/bin/qe-agent"
            />
          </label>
          <div class="mt-3 grid grid-cols-1 gap-2 rounded-lg border border-line bg-canvas/60 px-3 py-2.5 sm:grid-cols-3">
            <label class="flex items-center gap-2 text-[10.5px] text-muted">
              <input
                v-model="allowPoweroff"
                type="checkbox"
                class="h-3.5 w-3.5 rounded border-line text-accent focus:ring-accent/25"
              />
              允许远程关机
            </label>
            <label class="flex items-center justify-between gap-2 text-[10.5px] text-muted">
              遥测 fast
              <span class="flex items-center gap-1">
                <input
                  v-model="fastInterval"
                  type="number"
                  min="1"
                  max="60"
                  class="input h-7 w-[64px] text-right"
                />
                秒
              </span>
            </label>
            <label class="flex items-center justify-between gap-2 text-[10.5px] text-muted">
              遥测 full
              <span class="flex items-center gap-1">
                <input
                  v-model="fullInterval"
                  type="number"
                  min="60"
                  max="3600"
                  class="input h-7 w-[70px] text-right"
                />
                秒
              </span>
            </label>
            <label class="flex items-center gap-2 text-[10.5px] text-muted">
              <input
                v-model="livePush"
                type="checkbox"
                class="h-3.5 w-3.5 rounded border-line text-accent focus:ring-accent/25"
              />
              实时中转
            </label>
            <label class="flex items-center justify-between gap-2 text-[10.5px] text-muted">
              本地采样
              <span class="flex items-center gap-1">
                <input
                  v-model="liveIntervalMs"
                  type="number"
                  min="0"
                  max="5000"
                  step="100"
                  class="input h-7 w-[70px] text-right"
                />
                ms
              </span>
            </label>
          </div>
          <p class="mt-1.5 text-[9.5px] leading-4 text-faint">
            允许远程关机默认开启，会写入 <span class="mono">QE_AGENT_ALLOW_POWEROFF=1</span>；
            不需要时取消勾选。实时中转关闭时，样本仍按“本地采样”间隔写入本地缓冲，
            随下一次周期快照聚合提交（sections.live_metrics）。
          </p>

          <div class="mt-3 flex flex-wrap items-center gap-2">
            <button
              type="button"
              class="btn btn-sm btn-primary"
              :disabled="!installReady || busy !== null"
              @click="install"
            >
              <Download :size="12" class="mr-1" />
              {{ busy === 'install' ? '安装中…' : '安装并启动 daemon' }}
            </button>
            <button type="button" class="btn btn-sm" :disabled="busy !== null" @click="runProbe">
              <RefreshCw :size="12" class="mr-1" />
              重新检测
            </button>
            <span class="text-[9.5px] text-faint">
              {{ probe?.pkexec ? '将以管理员权限执行' : '缺少 pkexec，请使用 sudo 命令' }}
            </span>
          </div>

          <p v-if="!probe?.pkexec" class="mono mt-2 break-all rounded bg-canvas px-2 py-1.5 text-[9.5px] text-muted">
            {{ manualCommand }}
          </p>
          <p v-if="error" class="mt-2 rounded border border-danger/30 bg-danger-soft px-2 py-1.5 text-[10px] text-danger">
            {{ error }}
          </p>
          <pre v-if="result?.stdout" class="mono mt-2 max-h-24 overflow-auto rounded bg-canvas px-2 py-1.5 text-[9.5px] text-muted">{{ result.stdout }}</pre>
        </div>

        <!-- 需要注册 -->
        <div v-else-if="phase === 'enroll'" class="mt-5">
          <div class="rounded-lg border border-line bg-canvas px-3 py-2.5">
            <p class="flex items-center gap-2 text-[11.5px] font-medium text-ink">
              <Link2 :size="14" class="text-accent" />
              下一步注册设备
              <span
                class="ml-auto inline-flex items-center gap-1 text-[9.5px]"
                :class="daemonConnected ? 'text-emerald-600' : 'text-danger'"
              >
                <span class="h-1.5 w-1.5 rounded-full" :class="daemonConnected ? 'bg-emerald-500' : 'bg-danger'" />
                {{ phase === 'enroll' && !daemonConnected ? '待注册启动' : daemonConnected ? 'daemon 已连接' : 'daemon 未连接/已退出' }}
              </span>
            </p>
            <p class="mt-1 text-[10px] leading-4 text-muted">
              在管理端「房间/设备」页生成注册链接，粘贴到下方完成绑定。
            </p>
          </div>
          <input
            v-model="enrollLink"
            class="mono mt-3 w-full rounded border border-line bg-canvas px-2.5 py-1.5 text-[11px] text-ink outline-none focus:border-accent"
            placeholder="https://control.example.com/api/v1/device-pki/enroll?token=…"
          />
          <div class="mt-3 flex items-center gap-2">
            <button type="button" class="btn btn-sm btn-primary" :disabled="busy !== null" @click="enroll">
              <Link2 :size="12" class="mr-1" />
              {{ busy === 'enroll' ? '注册中…' : '注册设备' }}
            </button>
            <button type="button" class="btn btn-sm" :disabled="busy !== null" @click="runProbe">重新检测</button>
          </div>
          <p v-if="enrollMessage" class="mt-2 text-[10px] text-muted">{{ enrollMessage }}</p>
        </div>

        <!-- 错误/浏览器预览 -->
        <div v-else-if="phase === 'error'" class="mt-5 rounded-lg border border-danger/30 bg-danger-soft px-3 py-2.5">
          <p class="flex items-center gap-2 text-[11px] font-medium text-ink">
            <AlertTriangle :size="14" class="text-danger" />
            无法读取本机环境
          </p>
          <p class="mono mt-1 break-all text-[9.5px] leading-4 text-danger">{{ error }}</p>
          <button type="button" class="btn btn-sm mt-2" @click="runProbe">重试检测</button>
        </div>
      </div>

      <div class="flex items-center justify-between border-t border-line px-5 py-2.5">
        <p class="text-[9.5px] text-faint">检测系统 … 安装 daemon … 注册设备</p>
        <p class="text-[9.5px] text-faint">初始化完成后自动进入状态页</p>
      </div>
    </section>
  </div>
</template>
