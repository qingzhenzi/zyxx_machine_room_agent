<script setup lang="ts">
import { nextTick, onMounted, onUnmounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

import BootScene from './components/BootScene.vue'
import EnrollScene from './components/EnrollScene.vue'
import IdleScene from './components/IdleScene.vue'
import MessageScene from './components/MessageScene.vue'
import ProgressScene from './components/ProgressScene.vue'
import { pollDisplayState, type DisplayState } from './lib/ipc'

const state = ref<DisplayState | null>(null)
const connected = ref(false)
const unlockOpen = ref(false)
const password = ref('')
const unlockError = ref('')
const unlocking = ref(false)
const passwordInput = ref<HTMLInputElement | null>(null)
let timer: number | undefined

/** 拦截会关闭/切走展示层的按键；调试解锁走 Ctrl+Alt+Shift+U + 密码。 */
function onKeydown(event: KeyboardEvent) {
  if (event.ctrlKey && event.altKey && event.shiftKey && event.key.toLowerCase() === 'u') {
    event.preventDefault()
    unlockOpen.value = true
    unlockError.value = ''
    void nextTick(() => passwordInput.value?.focus())
    return
  }
  if (event.key === 'Delete' || (event.altKey && event.key === 'F4')) {
    event.preventDefault()
  }
}

async function submitUnlock() {
  if (unlocking.value) return
  unlocking.value = true
  unlockError.value = ''
  try {
    await invoke('unlock_display', { password: password.value })
  } catch (error) {
    unlockError.value = error instanceof Error ? error.message : String(error)
    unlocking.value = false
  }
}

async function tick() {
  try {
    state.value = await pollDisplayState()
    connected.value = true
  } catch {
    // agent 不可达时保持当前画面并继续重试，不黑屏。
    connected.value = false
  }
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown, true)
  void tick()
  timer = window.setInterval(() => void tick(), 1000)
})

onUnmounted(() => {
  window.removeEventListener('keydown', onKeydown, true)
  if (timer !== undefined) window.clearInterval(timer)
})
</script>

<template>
  <div class="h-full w-full overflow-hidden bg-[#0c0e11] text-[#eef1f5]">
    <template v-if="state">
      <!-- agent 推送的模板优先；params 里没有文案时回退到阶段消息 -->
      <MessageScene
        v-if="state.display.template === 'message'"
        :params="state.display.params"
      />
      <EnrollScene v-else-if="state.display.template === 'enroll'" :params="state.display.params" />
      <ProgressScene
        v-else-if="state.display.template === 'progress'"
        :message="String(state.display.params.message ?? '')"
        :detail="String(state.display.params.detail ?? '')"
        :progress="typeof state.display.params.progress === 'number' ? state.display.params.progress : null"
        :connected="connected"
      />
      <IdleScene
        v-else-if="state.display.template === 'idle'"
        :message="String(state.display.params.message ?? '')"
        :detail="String(state.display.params.detail ?? '')"
        :connected="connected"
      />
      <BootScene
        v-else
        :message="String(state.display.params.message ?? '')"
        :detail="String(state.display.params.detail ?? '')"
        :connected="connected"
      />
    </template>

    <div
      v-if="unlockOpen"
      class="fixed inset-0 z-[9999] flex items-center justify-center bg-black/80 p-6"
    >
      <div class="w-full max-w-md rounded-xl border border-white/15 bg-[#161a20] p-5 shadow-2xl">
        <h2 class="text-lg font-semibold">调试解锁</h2>
        <p class="mt-1 text-xs text-white/55">
          输入 QE_BOOT_DISPLAY_DEBUG_PASSWORD 后停止展示层并退出。
        </p>
        <input
          ref="passwordInput"
          v-model="password"
          type="password"
          autocomplete="off"
          class="mt-4 w-full rounded-lg border border-white/15 bg-black/30 px-3 py-2 text-sm outline-none focus:border-white/40"
          placeholder="调试密码"
          @keydown.enter="submitUnlock"
        />
        <p v-if="unlockError" class="mt-2 text-xs text-red-300">{{ unlockError }}</p>
        <div class="mt-4 flex justify-end gap-2">
          <button
            type="button"
            class="rounded-lg border border-white/15 px-3 py-1.5 text-sm text-white/70"
            @click="unlockOpen = false"
          >
            取消
          </button>
          <button
            type="button"
            class="rounded-lg bg-white px-3 py-1.5 text-sm font-medium text-black disabled:opacity-50"
            :disabled="unlocking"
            @click="submitUnlock"
          >
            {{ unlocking ? '解锁中…' : '解锁并退出' }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
