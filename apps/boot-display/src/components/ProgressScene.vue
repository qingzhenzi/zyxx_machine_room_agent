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
  <div class="flex h-full flex-col items-center justify-center">
    <div class="w-[min(46vw,52rem)]">
      <div class="rise flex items-baseline justify-between">
        <h1 class="text-[2vw] font-medium leading-tight">{{ message }}</h1>
        <span v-if="!indeterminate" class="text-[1.5vw] tabular-nums text-[var(--ink-soft)]">
          {{ Math.round(clamped) }}<span class="text-[0.9vw] text-[var(--ink-faint)]">%</span>
        </span>
      </div>
      <p v-if="detail" class="rise rise-1 mt-[1vw] text-[1.05vw] text-[var(--ink-faint)]">
        {{ detail }}
      </p>
      <div class="rise rise-2 mt-[2.4vw] h-[0.35vw] min-h-[3px] overflow-hidden rounded-full bg-[var(--line)]">
        <div
          v-if="indeterminate"
          class="sweep-bar h-full w-1/4 rounded-full bg-[var(--blue)]"
        />
        <div
          v-else
          class="h-full rounded-full bg-[var(--blue)] transition-[width] duration-500 ease-out"
          :style="{ width: `${clamped}%` }"
        />
      </div>
    </div>
    <div
      v-if="!connected"
      class="absolute bottom-[2.2vw] text-[0.85vw] text-[var(--ink-faint)] opacity-70"
    >
      等待 agent 连接…
    </div>
  </div>
</template>
