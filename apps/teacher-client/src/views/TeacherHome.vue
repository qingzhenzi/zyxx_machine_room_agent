<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Cpu, Monitor, Play, Power, RefreshCw, RotateCcw, Server, Settings, Square, Users } from 'lucide-vue-next'

import BrandMark from '@/components/ui/BrandMark.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'

interface MachineRow {
  serial: string
  device_slot?: string | null
  status?: string
  online?: string | null
  session_online?: string | null
  room_id?: string | null
  room_code?: string | null
  room_name?: string | null
  last_seen_at?: string | null
}
interface RoomRow { id: string; code: string; name: string; device_slot?: string | null }
interface VmRow { name: string; state?: string; vcpus?: number; memory_bytes?: number }
interface ContainerRow { name: string; state?: string; status?: string }

const controlUrl = ref(localStorage.getItem('qe.teacher.control') ?? 'http://192.168.8.231:8080')
const username = ref('admin')
const password = ref('')
const token = ref(localStorage.getItem('qe.teacher.token') ?? '')
const remember = ref(localStorage.getItem('qe.teacher.remember') !== 'false')
const busy = ref(false)
const error = ref('')
const rooms = ref<RoomRow[]>([])
const machines = ref<MachineRow[]>([])
const selectedRoom = ref(localStorage.getItem('qe.teacher.room') ?? '')
const selectedMachine = ref('')
const vms = ref<VmRow[]>([])
const containers = ref<ContainerRow[]>([])
const detailBusy = ref(false)

const loggedIn = computed(() => token.value.length > 0)
const currentRoom = computed(() => rooms.value.find((room) => room.id === selectedRoom.value))
const roomMachines = computed(() => machines.value.filter((machine) => !selectedRoom.value || machine.room_id === selectedRoom.value))
const currentMachine = computed(() => roomMachines.value.find((machine) => machine.serial === selectedMachine.value))
const onlineCount = computed(() => roomMachines.value.filter((machine) => isOnline(machine)).length)

async function api(path: string, init: RequestInit = {}) {
  const response = await fetch(`${controlUrl.value.replace(/\/$/, '')}${path}`, {
    ...init,
    headers: {
      'content-type': 'application/json',
      ...(token.value ? { authorization: `Bearer ${token.value}` } : {}),
      ...(init.headers ?? {}),
    },
  })
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`)
  return response.json()
}

async function login() {
  busy.value = true
  error.value = ''
  try {
    const reply = await api('/api/v1/auth/login', {
      method: 'POST',
      body: JSON.stringify({ username: username.value, password: password.value }),
    })
    token.value = reply.access_token
    localStorage.setItem('qe.teacher.token', token.value)
    localStorage.setItem('qe.teacher.control', controlUrl.value)
    localStorage.setItem('qe.teacher.remember', remember.value ? 'true' : 'false')
    if (remember.value) {
      localStorage.setItem('qe.teacher.username', username.value)
      localStorage.setItem('qe.teacher.password', password.value)
    } else {
      localStorage.removeItem('qe.teacher.username')
      localStorage.removeItem('qe.teacher.password')
    }
    await refresh()
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
    token.value = ''
  } finally {
    busy.value = false
  }
}

async function refresh() {
  if (!loggedIn.value) return
  busy.value = true
  error.value = ''
  try {
    const [roomPage, machinePage] = await Promise.all([
      api('/api/v1/admin/rooms?page_size=100'),
      api('/api/v1/admin/machines?page_size=100'),
    ])
    rooms.value = roomPage.items ?? []
    machines.value = machinePage.items ?? []
    if (!selectedRoom.value || !rooms.value.some((room) => room.id === selectedRoom.value)) {
      const b104 = rooms.value.find((room) => room.code.toLowerCase().includes('b104'))
      selectedRoom.value = b104?.id ?? rooms.value[0]?.id ?? ''
    }
    if (!selectedMachine.value || !roomMachines.value.some((machine) => machine.serial === selectedMachine.value)) {
      selectedMachine.value = roomMachines.value[0]?.serial ?? ''
    }
    if (selectedMachine.value) await loadMachine(selectedMachine.value)
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    busy.value = false
  }
}

async function sendCommand(kind: 'vm' | 'container', op: string, args: Record<string, unknown> = {}) {
  if (!selectedMachine.value) throw new Error('请选择机器')
  const base = `/api/v1/admin/machines/${encodeURIComponent(selectedMachine.value)}/${kind}/commands`
  const accepted = await api(base, { method: 'POST', body: JSON.stringify({ op, args }) })
  const requestId = accepted.request_id as string
  for (let i = 0; i < 40; i += 1) {
    await new Promise((resolve) => window.setTimeout(resolve, 500))
    const reply = await api(`${base}/${requestId}`)
    if (reply.status !== 'pending') {
      if (reply.error) throw new Error(String(reply.error))
      return reply.data
    }
  }
  throw new Error('命令超时')
}

async function loadMachine(serial: string) {
  selectedMachine.value = serial
  localStorage.setItem('qe.teacher.room', selectedRoom.value)
  detailBusy.value = true
  error.value = ''
  vms.value = []
  containers.value = []
  try {
    const vmData = await sendCommand('vm', 'list')
    vms.value = (vmData?.vms ?? []) as VmRow[]
    const containerData = await sendCommand('container', 'list')
    containers.value = (containerData?.containers ?? containerData?.items ?? []) as ContainerRow[]
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    detailBusy.value = false
  }
}

async function runVm(op: string, name: string) {
  detailBusy.value = true
  try { await sendCommand('vm', op, { name }); await loadMachine(selectedMachine.value) }
  catch (caught) { error.value = caught instanceof Error ? caught.message : String(caught) }
  finally { detailBusy.value = false }
}

async function runContainer(op: string, name: string) {
  detailBusy.value = true
  try { await sendCommand('container', op, { name }); await loadMachine(selectedMachine.value) }
  catch (caught) { error.value = caught instanceof Error ? caught.message : String(caught) }
  finally { detailBusy.value = false }
}

function isOnline(machine: MachineRow) { return machine.online === 'online' || machine.session_online === 'online' }
function stateLabel(state?: string) { return state ?? '未知' }
function formatMemory(bytes?: number) { return bytes ? `${Math.round(bytes / 1024 / 1024 / 1024)} GB` : '—' }
function shortSerial(serial: string) { return serial.length > 14 ? `${serial.slice(0, 8)}…${serial.slice(-4)}` : serial }
function selectMachine(serial: string) { if (!detailBusy.value) void loadMachine(serial) }

onMounted(() => {
  const savedUser = localStorage.getItem('qe.teacher.username')
  const savedPass = localStorage.getItem('qe.teacher.password')
  if (remember.value && savedUser && savedPass) {
    username.value = savedUser
    password.value = savedPass
    void login()
    return
  }
  if (loggedIn.value) void refresh()
})
</script>

<template>
  <div class="grid h-dvh grid-rows-[minmax(0,1fr)] overflow-hidden bg-canvas text-ink">
    <div class="mx-auto flex h-full w-full max-w-[1280px] flex-col px-6 py-5">
      <header class="mb-4 flex items-start gap-3">
        <BrandMark :size="38" />
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h1 class="text-[20px] font-semibold tracking-[-0.02em]">QEStudio 教师工作台</h1>
            <StatusBadge :label="loggedIn ? '已登录' : '未登录'" :tone="loggedIn ? 'success' : 'muted'" />
          </div>
          <p class="mt-0.5 text-[11px] text-muted">单房间 · 机器管理 · 虚拟机/容器控制</p>
        </div>
        <button class="btn btn-sm" :disabled="busy || !loggedIn" @click="refresh">
          <RefreshCw :size="14" /> 刷新
        </button>
      </header>

      <section v-if="!loggedIn" class="flex flex-1 items-center justify-center">
        <div class="panel w-full max-w-[560px] p-5">
          <div class="mb-4 flex items-center gap-2 text-[13px] font-semibold"><Settings :size="16" /> 登录控制面</div>
          <div class="grid gap-3">
            <label class="grid gap-1 text-[11px] text-muted">
              控制面地址
              <input v-model="controlUrl" class="rounded-md border border-line bg-surface-2 px-3 py-2 text-[12px] text-ink outline-none focus:border-accent-line" />
            </label>
            <div class="grid grid-cols-2 gap-3">
              <label class="grid gap-1 text-[11px] text-muted">
                用户名
                <input v-model="username" class="rounded-md border border-line bg-surface-2 px-3 py-2 text-[12px] text-ink outline-none focus:border-accent-line" />
              </label>
              <label class="grid gap-1 text-[11px] text-muted">
                密码
                <input v-model="password" type="password" class="rounded-md border border-line bg-surface-2 px-3 py-2 text-[12px] text-ink outline-none focus:border-accent-line" @keydown.enter="login" />
              </label>
            </div>
            <div class="flex items-center justify-between">
              <label class="flex items-center gap-2 text-[11px] text-muted"><input v-model="remember" type="checkbox" class="accent-[var(--accent)]" />自动登录</label>
              <button class="btn btn-primary" :disabled="busy" @click="login">{{ busy ? '登录中…' : '登录' }}</button>
            </div>
          </div>
        </div>
      </section>

      <template v-else>
        <p v-if="error" class="mb-3 rounded-md border border-danger/30 bg-danger-soft px-3 py-2 text-[11px] text-danger">{{ error }}</p>

        <section class="mb-4 grid gap-3 sm:grid-cols-3">
          <div class="panel p-4"><div class="flex items-center gap-2 text-[11px] text-muted"><Server :size="14" /> 管理房间</div><p class="mt-2 text-[16px] font-semibold">{{ currentRoom?.code ?? '—' }}</p><p class="text-[11px] text-muted">{{ currentRoom?.name ?? '选择房间' }}</p></div>
          <div class="panel p-4"><div class="flex items-center gap-2 text-[11px] text-muted"><Users :size="14" /> 机器</div><p class="mt-2 text-[16px] font-semibold">{{ roomMachines.length }}</p><p class="text-[11px] text-muted">在线 {{ onlineCount }} 台</p></div>
          <div class="panel p-4"><div class="flex items-center gap-2 text-[11px] text-muted"><Cpu :size="14" /> 实验环境</div><p class="mt-2 text-[16px] font-semibold">{{ vms.length }} VM · {{ containers.length }} 容器</p><p class="text-[11px] text-muted">{{ currentMachine?.device_slot ?? '未选择机器' }}</p></div>
        </section>

        <section class="mb-4 flex flex-wrap items-end gap-3 rounded-lg border border-line bg-surface p-3">
          <label class="grid gap-1 text-[11px] text-muted">
            当前房间
            <select v-model="selectedRoom" class="min-w-[260px] rounded-md border border-line bg-surface-2 px-3 py-2 text-[12px] text-ink outline-none focus:border-accent-line" @change="refresh">
              <option v-for="room in rooms" :key="room.id" :value="room.id">{{ room.code }} · {{ room.name }}</option>
            </select>
          </label>
          <span class="pb-2 text-[11px] text-muted">仅显示该房间设备</span>
        </section>

        <div class="grid min-h-0 flex-1 gap-4 lg:grid-cols-[300px_minmax(0,1fr)]">
          <section class="min-h-0 overflow-y-auto">
            <div class="mb-2 flex items-center justify-between text-[11px] text-muted"><span>机器列表</span><span>{{ roomMachines.length }} 台</span></div>
            <button
              v-for="machine in roomMachines"
              :key="machine.serial"
              type="button"
              class="mb-2 w-full rounded-lg border p-3 text-left transition-colors"
              :class="machine.serial === selectedMachine ? 'border-accent-line bg-accent-soft' : 'border-line bg-surface hover:border-line-strong'"
              @click="selectMachine(machine.serial)"
            >
              <div class="flex items-center justify-between gap-2">
                <span class="truncate text-[12px] font-medium">{{ machine.device_slot ?? shortSerial(machine.serial) }}</span>
                <StatusBadge :label="isOnline(machine) ? '在线' : '离线'" :tone="isOnline(machine) ? 'success' : 'muted'" />
              </div>
              <p class="mt-1 truncate text-[11px] text-muted">{{ machine.room_name ?? '未绑定房间' }}</p>
              <p class="mt-0.5 truncate font-mono text-[10px] text-faint">{{ shortSerial(machine.serial) }}</p>
            </button>
            <p v-if="roomMachines.length === 0" class="panel p-4 text-[11px] text-muted">该房间暂无机器</p>
          </section>

          <section class="min-h-0 space-y-4 overflow-y-auto">
            <div class="panel overflow-hidden">
              <div class="panel-header">
                <div><h2 class="panel-title">虚拟机</h2><p class="panel-subtitle">{{ selectedMachine ? shortSerial(selectedMachine) : '未选择机器' }}</p></div>
                <button class="btn btn-sm" :disabled="detailBusy || !selectedMachine" @click="loadMachine(selectedMachine)"><RefreshCw :size="13" /> 刷新</button>
              </div>
              <div class="space-y-2 p-3">
                <div v-for="vm in vms" :key="vm.name" class="flex flex-wrap items-center justify-between gap-2 rounded-md border border-line bg-surface-2 p-3">
                  <div class="min-w-0">
                    <div class="flex flex-wrap items-center gap-2"><span class="text-[12px] font-medium">{{ vm.name }}</span><StatusBadge :label="stateLabel(vm.state)" :tone="vm.state === 'running' ? 'success' : 'muted'" /></div>
                    <p class="mt-1 text-[10px] text-muted">{{ vm.vcpus ?? '—' }} vCPU · {{ formatMemory(vm.memory_bytes) }}</p>
                  </div>
                  <div class="flex gap-1">
                    <button class="btn btn-sm" :disabled="detailBusy" @click="runVm('start', vm.name)"><Play :size="12" /> 启动</button>
                    <button class="btn btn-sm" :disabled="detailBusy" @click="runVm('shutdown', vm.name)"><Power :size="12" /> 关机</button>
                    <button class="btn btn-sm btn-danger" :disabled="detailBusy" @click="runVm('destroy', vm.name)"><Square :size="12" /> 强制停止</button>
                  </div>
                </div>
                <div v-if="vms.length === 0" class="flex items-center gap-2 p-3 text-[11px] text-muted"><Monitor :size="14" /> 没有虚拟机</div>
              </div>
            </div>

            <div class="panel overflow-hidden">
              <div class="panel-header"><div><h2 class="panel-title">容器</h2><p class="panel-subtitle">Incus 容器</p></div></div>
              <div class="space-y-2 p-3">
                <div v-for="container in containers" :key="container.name" class="flex flex-wrap items-center justify-between gap-2 rounded-md border border-line bg-surface-2 p-3">
                  <div class="flex min-w-0 items-center gap-2"><span class="text-[12px] font-medium">{{ container.name }}</span><StatusBadge :label="stateLabel(container.state ?? container.status)" :tone="(container.state ?? container.status) === 'running' ? 'success' : 'muted'" /></div>
                  <div class="flex gap-1">
                    <button class="btn btn-sm" :disabled="detailBusy" @click="runContainer('start', container.name)"><Play :size="12" /> 启动</button>
                    <button class="btn btn-sm" :disabled="detailBusy" @click="runContainer('stop', container.name)"><Square :size="12" /> 停止</button>
                    <button class="btn btn-sm" :disabled="detailBusy" @click="runContainer('restart', container.name)"><RotateCcw :size="12" /> 重启</button>
                  </div>
                </div>
                <div v-if="containers.length === 0" class="flex items-center gap-2 p-3 text-[11px] text-muted"><Server :size="14" /> 没有容器</div>
              </div>
            </div>
          </section>
        </div>
      </template>
    </div>
  </div>
</template>
