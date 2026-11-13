<script setup lang="ts">
import { computed } from 'vue'

import type { Tone } from '@/lib/status'

const props = withDefaults(
  defineProps<{
    /** Already-translated label. */
    label: string
    tone?: Tone
    pulse?: boolean
    dot?: boolean
  }>(),
  { tone: 'muted', pulse: false, dot: true },
)

const showPulse = computed(() => props.pulse || (props.tone === 'accent' && props.dot))
</script>

<template>
  <span class="status-badge" :class="`status-badge-${tone}`">
    <span v-if="dot" class="status-dot" :class="{ 'animate-pulse-dot': showPulse }" />
    {{ label }}
  </span>
</template>
