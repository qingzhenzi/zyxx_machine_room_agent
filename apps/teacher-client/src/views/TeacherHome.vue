<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

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
const roomMachines = computed(() => machines.value.filter((m) => !selectedRoom.value || m.room_id === selectedRoom.value))

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
      api('/api/v1/admin/machines?page_size=200'),
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

function stateLabel(state?: string) { return state ?? '未知' }
function machineOnline(machine: MachineRow) { return machine.online === 'online' || machine.session_online === 'online' }
function formatMemory(bytes?: number) { return bytes ? `${Math.round(bytes / 1024 / 1024 / 1024)} GB` : '—' }

onMounted(() => { if (loggedIn.value) void refresh() })
</script>

<template>
  <div class="min-h-dvh bg-slate-950 text-slate-100">
    <header class="border-b border-white/10 px-6 py-4">
      <div class="mx-auto flex max-w-7xl items-center justify-between">
        <div>
          <h1 class="text-lg font-semibold">QEStudio 教师工作台</h1>
          <p class="text-xs text-slate-400">单房间 · 机器管理 · 虚拟机/容器控制</p>
        </div>
        <span class="rounded-full border border-white/10 px-3 py-1 text-xs text-slate-300">{{ loggedIn ? '已登录' : '未登录' }}</span>
      </div>
    </header>

    <main class="mx-auto max-w-7xl space-y-4 p-6">
      <section v-if="!loggedIn" class="rounded-xl border border-white/10 bg-slate-900 p-5">
        <h2 class="mb-3 text-sm font-semibold">登录控制面</h2>
        <div class="grid gap-3 md:grid-cols-4">
          <label class="text-xs text-slate-400">控制面地址<input v-model="controlUrl" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" /></label>
          <label class="text-xs text-slate-400">用户名<input v-model="username" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" /></label>
          <label class="text-xs text-slate-400">密码<input v-model="password" type="password" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" /></label>
          <button class="mt-5 h-9 rounded bg-cyan-500 px-4 text-sm font-medium text-slate-950 disabled:opacity-50" :disabled="busy" @click="login">登录</button>
        </div>
      </section>

      <p v-if="error" class="rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200">{{ error }}</p>

      <template v-if="loggedIn">
        <section class="flex flex-wrap items-end gap-3 rounded-xl border border-white/10 bg-slate-900 p-4">
          <label class="text-xs text-slate-400">管理房间
            <select v-model="selectedRoom" class="mt-1 block min-w-64 rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" @change="refresh">
              <option v-for="room in rooms" :key="room.id" :value="room.id">{{ room.code }} · {{ room.name }}</option>
            </select>
          </label>
          <button class="h-9 rounded border border-white/15 px-3 text-xs" :disabled="busy" @click="refresh">刷新</button>
          <span class="text-xs text-slate-400">该房间 {{ roomMachines.length }} 台机器</span>
        </section>

        <div class="grid gap-4 lg:grid-cols-[300px_minmax(0,1fr)]">
          <section class="space-y-2">
            <button
              v-for="machine in roomMachines"
              :key="machine.serial"
              class="w-full rounded-xl border p-3 text-left text-xs transition"
              :class="machine.serial === selectedMachine ? 'border-cyan-400/60 bg-cyan-500/10' : 'border-white/10 bg-slate-900 hover:border-white/25'"
              @click="loadMachine(machine.serial)"
            >
              <div class="flex items-center justify-between gap-2">
                <span class="font-medium">{{ machine.device_slot ?? machine.serial.slice(0, 12) }}</span>
                <span :class="machineOnline(machine) ? 'text-emerald-300' : 'text-slate-500'">{{ machineOnline(machine) ? '在线' : '离线' }}</span>
              </div>
              <p class="mt-1 truncate text-slate-400">{{ machine.room_name ?? '未绑定房间' }}</p>
              <p class="mt-1 text-slate-500">{{ machine.last_seen_at ?? '无心跳' }}</p>
            </button>
            <p v-if="roomMachines.length === 0" class="rounded border border-white/10 p-3 text-xs text-slate-500">该房间暂无机器</p>
          </section>

          <section class="space-y-4">
            <div class="rounded-xl border border-white/10 bg-slate-900 p-4">
              <div class="flex items-center justify-between">
                <div>
                  <h2 class="text-sm font-semibold">虚拟机</h2>
                  <p class="text-xs text-slate-400">{{ selectedMachine || '未选择机器' }}</p>
                </div>
                <button class="rounded border border-white/15 px-3 py-1 text-xs" :disabled="detailBusy" @click="loadMachine(selectedMachine)">刷新</button>
              </div>
              <div class="mt-3 space-y-2">
                <div v-for="vm in vms" :key="vm.name" class="flex flex-wrap items-center justify-between gap-2 rounded border border-white/10 bg-slate-950 p-3 text-xs">
                  <div>
                    <span class="font-medium">{{ vm.name }}</span>
                    <span class="ml-2 text-slate-400">{{ stateLabel(vm.state) }}</span>
                    <span class="ml-2 text-slate-500">{{ vm.vcpus ?? '—' }} vCPU · {{ formatMemory(vm.memory_bytes) }}</span>
                  </div>
                  <div class="flex gap-1">
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runVm('start', vm.name)">启动</button>
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runVm('shutdown', vm.name)">关机</button>
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runVm('destroy', vm.name)">强制停止</button>
                  </div>
                </div>
                <p v-if="vms.length === 0" class="text-xs text-slate-500">没有虚拟机</p>
              </div>
            </div>

            <div class="rounded-xl border border-white/10 bg-slate-900 p-4">
              <h2 class="text-sm font-semibold">容器</h2>
              <div class="mt-3 space-y-2">
                <div v-for="container in containers" :key="container.name" class="flex flex-wrap items-center justify-between gap-2 rounded border border-white/10 bg-slate-950 p-3 text-xs">
                  <div><span class="font-medium">{{ container.name }}</span><span class="ml-2 text-slate-400">{{ stateLabel(container.state ?? container.status) }}</span></div>
                  <div class="flex gap-1">
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runContainer('start', container.name)">启动</button>
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runContainer('stop', container.name)">停止</button>
                    <button class="rounded border border-white/15 px-2 py-1" :disabled="detailBusy" @click="runContainer('restart', container.name)">重启</button>
                  </div>
                </div>
                <p v-if="containers.length === 0" class="text-xs text-slate-500">没有容器</p>
              </div>
            </div>
          </section>
        </div>
      </template>
    </main>
  </div>
</template>
