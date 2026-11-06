<script setup lang="ts">
defineProps<{
  message: string
  detail: string
  connected: boolean
}>()
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center gap-8">
    <div class="flex flex-col items-center gap-4">
      <div class="text-5xl font-semibold tracking-wide">QEStudio</div>
      <div class="h-8 w-px animate-pulse bg-[#e8b04b]" aria-hidden="true" />
    </div>
    <div class="flex flex-col items-center gap-2">
      <p class="text-lg text-[#c3cad4]">{{ message }}</p>
      <p v-if="detail" class="text-sm text-[#8a94a3]">{{ detail }}</p>
    </div>
    <div
      class="absolute bottom-4 text-xs"
      :class="connected ? 'text-[#4ecb8d]' : 'text-[#f06a6a]'"
    >
      {{ connected ? 'agent 已连接' : '等待 agent…' }}
    </div>
  </div>
</template>
