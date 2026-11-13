<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

interface ImageRow { id: string; name: string; version: string; status: string; kind?: string; format?: string; platform?: string; total_size?: number | null }
interface RoomRow { id: string; code: string; name: string; device_slot?: string | null }
interface MachineRow { serial: string; hostname?: string; device_slot?: string | null; online?: string; status?: string }

const controlUrl = ref(localStorage.getItem('qe.teacher.control') ?? 'http://192.168.8.231:8080')
const username = ref('admin')
const password = ref('')
const token = ref(localStorage.getItem('qe.teacher.token') ?? '')
const busy = ref(false)
const error = ref('')
const images = ref<ImageRow[]>([])
const rooms = ref<RoomRow[]>([])
const machines = ref<MachineRow[]>([])
const selectedImage = ref('')
const selectedRoom = ref('')

const loggedIn = computed(() => token.value.length > 0)

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
    const [imagePage, roomPage, machinePage] = await Promise.all([
      api('/api/v1/admin/images?page_size=100'),
      api('/api/v1/admin/rooms?page_size=100'),
      api('/api/v1/admin/machines?page_size=100'),
    ])
    images.value = imagePage.items ?? []
    rooms.value = roomPage.items ?? []
    machines.value = machinePage.items ?? []
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    busy.value = false
  }
}

async function assignImage() {
  if (!selectedImage.value || !selectedRoom.value) return
  busy.value = true
  error.value = ''
  try {
    await api(`/api/v1/admin/rooms/${selectedRoom.value}/images/${selectedImage.value}`, {
      method: 'PUT',
      body: JSON.stringify({}),
    })
    await refresh()
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    busy.value = false
  }
}

onMounted(() => { if (loggedIn.value) void refresh() })
</script>

<template>
  <div class="min-h-dvh bg-slate-950 text-slate-100">
    <header class="border-b border-white/10 px-6 py-4">
      <div class="mx-auto flex max-w-6xl items-center justify-between">
        <div>
          <h1 class="text-lg font-semibold">QEStudio 教师工作台</h1>
          <p class="text-xs text-slate-400">机器状态 · 镜像中心 · 镜像下发</p>
        </div>
        <span class="rounded-full border border-white/10 px-3 py-1 text-xs text-slate-300">
          {{ loggedIn ? '已登录' : '未登录' }}
        </span>
      </div>
    </header>

    <main class="mx-auto max-w-6xl space-y-4 p-6">
      <section v-if="!loggedIn" class="rounded-xl border border-white/10 bg-slate-900 p-5">
        <h2 class="mb-3 text-sm font-semibold">登录控制面</h2>
        <div class="grid gap-3 md:grid-cols-4">
          <label class="text-xs text-slate-400">控制面地址
            <input v-model="controlUrl" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" />
          </label>
          <label class="text-xs text-slate-400">用户名
            <input v-model="username" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" />
          </label>
          <label class="text-xs text-slate-400">密码
            <input v-model="password" type="password" class="mt-1 w-full rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm" />
          </label>
          <button class="mt-5 h-9 rounded bg-cyan-500 px-4 text-sm font-medium text-slate-950 disabled:opacity-50" :disabled="busy" @click="login">登录</button>
        </div>
      </section>

      <p v-if="error" class="rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200">{{ error }}</p>

      <template v-if="loggedIn">
        <section class="grid gap-4 md:grid-cols-3">
          <div class="rounded-xl border border-white/10 bg-slate-900 p-4"><p class="text-xs text-slate-400">镜像</p><p class="mt-1 text-2xl font-semibold">{{ images.length }}</p></div>
          <div class="rounded-xl border border-white/10 bg-slate-900 p-4"><p class="text-xs text-slate-400">房间</p><p class="mt-1 text-2xl font-semibold">{{ rooms.length }}</p></div>
          <div class="rounded-xl border border-white/10 bg-slate-900 p-4"><p class="text-xs text-slate-400">机器</p><p class="mt-1 text-2xl font-semibold">{{ machines.length }}</p></div>
        </section>

        <section class="rounded-xl border border-white/10 bg-slate-900">
          <div class="flex items-center justify-between border-b border-white/10 px-4 py-3">
            <h2 class="text-sm font-semibold">镜像中心</h2>
            <button class="rounded border border-white/15 px-3 py-1 text-xs" :disabled="busy" @click="refresh">刷新</button>
          </div>
          <div class="max-h-80 overflow-auto">
            <table class="w-full text-left text-xs">
              <thead class="text-slate-400"><tr><th class="p-3">名称</th><th class="p-3">版本</th><th class="p-3">类型</th><th class="p-3">状态</th></tr></thead>
              <tbody>
                <tr v-for="image in images" :key="image.id" class="border-t border-white/5">
                  <td class="p-3">{{ image.name }}</td><td class="p-3">{{ image.version }}</td>
                  <td class="p-3">{{ image.kind ?? '—' }}</td><td class="p-3">{{ image.status }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>

        <section class="rounded-xl border border-white/10 bg-slate-900 p-4">
          <h2 class="mb-3 text-sm font-semibold">镜像下发</h2>
          <div class="grid gap-3 md:grid-cols-3">
            <select v-model="selectedImage" class="rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm">
              <option value="">选择镜像</option>
              <option v-for="image in images" :key="image.id" :value="image.id">{{ image.name }}:{{ image.version }}</option>
            </select>
            <select v-model="selectedRoom" class="rounded border border-white/10 bg-slate-950 px-3 py-2 text-sm">
              <option value="">选择房间</option>
              <option v-for="room in rooms" :key="room.id" :value="room.id">{{ room.code }} · {{ room.name }}</option>
            </select>
            <button class="rounded bg-cyan-500 px-4 py-2 text-sm font-medium text-slate-950 disabled:opacity-50" :disabled="busy || !selectedImage || !selectedRoom" @click="assignImage">下发到房间</button>
          </div>
        </section>
      </template>
    </main>
  </div>
</template>
