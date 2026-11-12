<script setup lang="ts">
import MiniSparkline from '@/components/ui/MiniSparkline.vue'
import type { Tone } from '@/lib/status'

withDefaults(
  defineProps<{
    label: string
    value: string
    delta?: string
    positive?: boolean
    note?: string
    trend?: number[]
    tone?: Tone
    size?: 'sm' | 'md'
  }>(),
  {
    positive: true,
    tone: 'accent',
    size: 'md',
  },
)
</script>

<template>
  <article class="panel flex flex-col justify-between px-3.5 pb-2.5 pt-3">
    <div class="flex items-start justify-between gap-2">
      <p class="text-[10.5px] font-medium text-muted">{{ label }}</p>
      <span
        v-if="delta"
        class="rounded px-1.5 py-px text-[9.5px] font-medium tabular-nums"
        :class="positive ? 'bg-success-soft text-success' : 'bg-danger-soft text-danger'"
      >
        {{ delta }}
      </span>
    </div>
    <div class="mt-1.5 flex items-end justify-between gap-2">
      <p
        class="font-semibold leading-none tracking-[-0.02em] tabular-nums text-ink"
        :class="size === 'sm' ? 'text-[17px]' : 'text-[21px]'"
      >
        {{ value }}
      </p>
      <MiniSparkline
        v-if="trend && trend.length"
        :values="trend"
        :tone="tone"
        :width="size === 'sm' ? 66 : 76"
        :height="size === 'sm' ? 28 : 24"
        :stroke-width="1.3"
      />
    </div>
    <p v-if="note" class="mt-1.5 text-[9.5px] text-faint">{{ note }}</p>
  </article>
</template>
