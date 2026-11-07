<script setup lang="ts">
defineProps<{
  message: string
  detail: string
  connected: boolean
}>()
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center">
    <div class="flex flex-col items-center">
      <div class="rise text-[4.2vw] font-medium tracking-[0.2em] leading-none">QESTUDIO</div>
      <div class="rise rise-1 mt-[3.2vw] h-px w-[22vw] overflow-hidden bg-[var(--line)]">
        <div class="sweep-bar h-px w-1/4 bg-[var(--blue)]" />
      </div>
      <p class="rise rise-2 mt-[3.2vw] text-[1.35vw] text-[var(--ink-soft)]">{{ message }}</p>
      <p v-if="detail" class="rise rise-3 mt-[0.9vw] text-[0.95vw] text-[var(--ink-faint)]">
        {{ detail }}
      </p>
    </div>
    <div
      v-if="!connected"
      class="absolute bottom-[2.2vw] text-[0.85vw] tracking-wide text-[var(--ink-faint)] opacity-70"
    >
      等待 agent 连接…
    </div>
  </div>
</template>
