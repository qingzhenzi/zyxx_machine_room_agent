<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  message: string
  detail: string
  progress: number | null
  connected: boolean
}>()

const clamped = computed(() => {
  if (props.progress === null) return 0
  return Math.min(100, Math.max(0, props.progress))
})

const indeterminate = computed(() => props.progress === null)
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center gap-10 px-[15vw]">
    <div class="text-center">
      <h1 class="text-3xl font-medium">{{ message }}</h1>
      <p v-if="detail" class="mt-3 text-sm text-[#8a94a3]">{{ detail }}</p>
    </div>
    <div class="relative h-1.5 w-full overflow-hidden rounded-full bg-[#171b21]">
      <div
        v-if="indeterminate"
        class="absolute inset-y-0 w-1/3 animate-[slide_1.4s_ease-in-out_infinite] rounded-full bg-[#e8b04b]"
      />
      <div
        v-else
        class="h-full rounded-full bg-[#e8b04b] transition-all duration-500"
        :style="{ width: `${clamped}%` }"
      />
    </div>
    <div class="absolute bottom-4 text-xs" :class="connected ? 'text-[#4ecb8d]' : 'text-[#f06a6a]'">
      {{ connected ? 'agent 已连接' : '等待 agent…' }}
    </div>
  </div>
</template>
