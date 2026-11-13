<script setup lang="ts">
import { computed } from 'vue'

import type { Tone } from '@/lib/status'

const props = withDefaults(
  defineProps<{
    values: number[]
    width?: number
    height?: number
    tone?: Tone
    filled?: boolean
    strokeWidth?: number
    showLast?: boolean
  }>(),
  {
    width: 88,
    height: 26,
    tone: 'accent',
    filled: true,
    strokeWidth: 1.4,
    showLast: true,
  },
)

const colors: Record<Tone, string> = {
  success: 'var(--success)',
  danger: 'var(--danger)',
  warning: 'var(--warning)',
  info: 'var(--info)',
  accent: 'var(--accent)',
  muted: 'var(--muted)',
}

const color = computed(() => colors[props.tone])

const geometry = computed(() => {
  const values = props.values.length > 1 ? props.values : [props.values[0] ?? 0, props.values[0] ?? 0]
  const min = Math.min(...values)
  const max = Math.max(...values)
  const range = max - min || 1
  const pad = 2
  const usableWidth = props.width - pad * 2
  const usableHeight = props.height - pad * 2

  const points = values.map((value, index) => {
    const x = pad + (index / (values.length - 1)) * usableWidth
    const y = pad + (1 - (value - min) / range) * usableHeight
    return { x, y }
  })

  const line = points.map((point) => `${point.x},${point.y}`).join(' ')
  const area = `${points[0]!.x},${props.height - pad} ${line} ${points[points.length - 1]!.x},${props.height - pad}`
  const last = points[points.length - 1]!

  return { line, area, last }
})
</script>

<template>
  <svg
    :width="width"
    :height="height"
    :viewBox="`0 0 ${width} ${height}`"
    fill="none"
    aria-hidden="true"
    class="shrink-0 overflow-visible"
    :style="{ color }"
  >
    <polygon
      v-if="filled"
      :points="geometry.area"
      fill="currentColor"
      fill-opacity="0.08"
    />
    <polyline
      :points="geometry.line"
      fill="none"
      stroke="currentColor"
      :stroke-width="strokeWidth"
      stroke-linecap="round"
      stroke-linejoin="round"
      vector-effect="non-scaling-stroke"
    />
    <circle
      v-if="showLast"
      :cx="geometry.last.x"
      :cy="geometry.last.y"
      r="2.2"
      fill="currentColor"
      stroke="var(--surface)"
      stroke-width="1.2"
    />
  </svg>
</template>
