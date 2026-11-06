<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'

import BootScene from './components/BootScene.vue'
import IdleScene from './components/IdleScene.vue'
import ProgressScene from './components/ProgressScene.vue'
import { pollDisplayState, type DisplayState } from './lib/ipc'

const state = ref<DisplayState>({
  scene: 'boot',
  message: '正在启动…',
  detail: '',
  progress: null,
})
const connected = ref(false)
let timer: number | undefined

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
  void tick()
  timer = window.setInterval(() => void tick(), 1000)
})

onUnmounted(() => {
  if (timer !== undefined) window.clearInterval(timer)
})
</script>

<template>
  <div class="h-full w-full overflow-hidden bg-[#0c0e11] text-[#eef1f5]">
    <BootScene v-if="state.scene === 'boot'" :message="state.message" :detail="state.detail" :connected="connected" />
    <ProgressScene
      v-else-if="state.scene === 'progress'"
      :message="state.message"
      :detail="state.detail"
      :progress="state.progress"
      :connected="connected"
    />
    <IdleScene v-else :message="state.message" :detail="state.detail" :connected="connected" />
  </div>
</template>
