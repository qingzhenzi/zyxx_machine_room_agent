<script setup lang="ts">
import type { Tone } from '@/lib/status'

withDefaults(
  defineProps<{
    label: string
    tone?: Tone
    pulse?: boolean
    dot?: boolean
  }>(),
  { tone: 'muted', pulse: false, dot: true },
)
</script>

<template>
  <span class="status-badge" :class="`status-badge-${tone}`">
    <span
      v-if="dot"
      class="status-dot"
      :class="{ 'animate-pulse-dot': pulse || tone === 'accent' }"
    />
    {{ label }}
  </span>
</template>
