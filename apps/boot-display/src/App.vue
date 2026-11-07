<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'

import BootScene from './components/BootScene.vue'
import EnrollScene from './components/EnrollScene.vue'
import IdleScene from './components/IdleScene.vue'
import MessageScene from './components/MessageScene.vue'
import ProgressScene from './components/ProgressScene.vue'
import { pollDisplayState, type DisplayState } from './lib/ipc'

const state = ref<DisplayState | null>(null)
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
  </div>
</template>
