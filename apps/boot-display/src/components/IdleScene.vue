<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  message: string
  detail: string
  connected: boolean
}>()

const clock = computed(() => {
  return new Date().toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
})
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center gap-6">
    <div class="text-7xl font-light tabular-nums">{{ clock }}</div>
    <p class="text-xl text-[#c3cad4]">{{ message || '设备就绪' }}</p>
    <p v-if="detail" class="text-sm text-[#8a94a3]">{{ detail }}</p>
    <div class="absolute bottom-4 text-xs" :class="connected ? 'text-[#4ecb8d]' : 'text-[#f06a6a]'">
      {{ connected ? 'agent 已连接' : '等待 agent…' }}
    </div>
  </div>
</template>
