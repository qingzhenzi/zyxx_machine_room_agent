<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'

const props = defineProps<{
  message: string
  detail: string
  connected: boolean
}>()

const now = ref(new Date())
let timer: number | undefined

onMounted(() => {
  timer = window.setInterval(() => {
    now.value = new Date()
  }, 1000)
})

onUnmounted(() => {
  if (timer !== undefined) window.clearInterval(timer)
})

const clock = computed(() =>
  now.value.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' }),
)
const date = computed(() =>
  now.value.toLocaleDateString('zh-CN', { month: 'long', day: 'numeric', weekday: 'long' }),
)
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center">
    <div class="rise text-[9vw] font-extralight leading-none tabular-nums tracking-[-0.02em]">
      {{ clock }}
    </div>
    <div class="rise rise-1 mt-[1.6vw] text-[1.3vw] text-[var(--ink-faint)]">{{ date }}</div>
    <p v-if="message" class="rise rise-2 mt-[5vw] text-[1.35vw] text-[var(--ink-soft)]">
      {{ message }}
    </p>
    <div
      v-if="!connected"
      class="absolute bottom-[2.2vw] text-[0.85vw] text-[var(--ink-faint)] opacity-70"
    >
      等待 agent 连接…
    </div>
  </div>
</template>
