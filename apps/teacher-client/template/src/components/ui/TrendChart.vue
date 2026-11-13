<script setup lang="ts">
import { computed } from 'vue'

import type { Tone } from '@/lib/status'

const props = withDefaults(
  defineProps<{
    values: number[]
    labels?: string[]
    height?: number
    tone?: Tone
    min?: number
    max?: number
    showGrid?: boolean
    showDots?: boolean
  }>(),
  {
    labels: () => [],
    height: 168,
    tone: 'accent',
    showGrid: true,
    showDots: true,
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

const chart = computed(() => {
  const values = props.values.length > 1 ? props.values : [0, props.values[0] ?? 0]
  const computedMin = props.min ?? Math.min(...values)
  const computedMax = props.max ?? Math.max(...values)
  const spread = computedMax - computedMin || 1
  const paddedMin = props.min ?? Math.max(0, computedMin - spread * 0.12)
  const paddedMax = props.max ?? computedMax + spread * 0.12
  const range = paddedMax - paddedMin || 1

  const left = 10
  const right = 590
  const top = 12
  const bottom = 148

  const points = values.map((value, index) => {
    const x = left + (index / (values.length - 1)) * (right - left)
    const y = bottom - ((value - paddedMin) / range) * (bottom - top)
    return { x, y, value }
  })

  const linePath = points
    .map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x.toFixed(2)} ${point.y.toFixed(2)}`)
    .join(' ')
  const areaPath = `${linePath} L ${right} ${bottom} L ${left} ${bottom} Z`
  const last = points[points.length - 1]!

  return { points, linePath, areaPath, last, paddedMin, paddedMax }
})
</script>

<template>
  <div class="w-full" :style="{ color }">
    <div class="relative w-full" :style="{ height: `${height}px` }">
      <svg
        class="absolute inset-0 h-full w-full overflow-visible"
        viewBox="0 0 600 160"
        preserveAspectRatio="none"
        fill="none"
        aria-hidden="true"
      >
        <g v-if="showGrid" stroke="var(--line)" stroke-width="1" vector-effect="non-scaling-stroke">
          <line x1="10" y1="12" x2="590" y2="12" stroke-opacity="0.7" />
          <line x1="10" y1="80" x2="590" y2="80" stroke-opacity="0.7" />
          <line x1="10" y1="148" x2="590" y2="148" stroke-opacity="0.9" />
        </g>
        <path :d="chart.areaPath" fill="currentColor" fill-opacity="0.08" />
        <path
          :d="chart.linePath"
          stroke="currentColor"
          stroke-width="1.8"
          stroke-linecap="round"
          stroke-linejoin="round"
          vector-effect="non-scaling-stroke"
        />
        <template v-if="showDots">
          <circle
            v-for="point in chart.points"
            :key="`${point.x}-${point.y}`"
            :cx="point.x"
            :cy="point.y"
            r="2"
            fill="var(--surface)"
            stroke="currentColor"
            stroke-width="1.4"
            vector-effect="non-scaling-stroke"
          />
        </template>
        <circle
          :cx="chart.last.x"
          :cy="chart.last.y"
          r="3.4"
          fill="currentColor"
          stroke="var(--surface)"
          stroke-width="1.6"
          vector-effect="non-scaling-stroke"
        />
      </svg>

    </div>
    <div v-if="labels.length" class="mt-1 flex justify-between pl-1 pr-5">
      <span
        v-for="(label, index) in labels"
        :key="`${label}-${index}`"
        class="text-[9.5px] text-faint"
      >
        {{ label }}
      </span>
    </div>
  </div>
</template>
