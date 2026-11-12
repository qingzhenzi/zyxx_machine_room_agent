<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    passed: number
    failed: number
    skipped: number
    size?: number
    thickness?: number
    label?: string
  }>(),
  {
    size: 128,
    thickness: 12,
    label: '通过率',
  },
)

const total = computed(() => props.passed + props.failed + props.skipped)
const passRate = computed(() => (total.value === 0 ? 0 : (props.passed / total.value) * 100))
const passEnd = computed(() => passRate.value)
const failedEnd = computed(() => passRate.value + (total.value === 0 ? 0 : (props.failed / total.value) * 100))

const ringBackground = computed(() => {
  if (total.value === 0) return 'var(--surface-3)'

  return `conic-gradient(var(--success) 0% ${passEnd.value}%, var(--danger) ${passEnd.value}% ${failedEnd.value}%, var(--faint) ${failedEnd.value}% 100%)`
})

const innerSize = computed(() => props.size - props.thickness * 2)
</script>

<template>
  <div
    class="relative shrink-0 rounded-full"
    :style="{
      width: `${size}px`,
      height: `${size}px`,
      background: ringBackground,
      boxShadow: 'inset 0 0 0 1px rgba(0,0,0,0.12)',
    }"
    role="img"
    :aria-label="`${label} ${passRate.toFixed(1)}%`"
  >
    <div
      class="absolute rounded-full bg-surface"
      :style="{
        width: `${innerSize}px`,
        height: `${innerSize}px`,
        top: `${thickness}px`,
        left: `${thickness}px`,
      }"
    />
    <div class="absolute inset-0 flex flex-col items-center justify-center">
      <span class="text-[19px] font-semibold tabular-nums tracking-[-0.02em] text-ink">
        {{ passRate.toFixed(passRate % 1 === 0 ? 0 : 1) }}%
      </span>
      <span class="mt-0.5 text-[9.5px] text-faint">{{ t(label) }}</span>
    </div>
  </div>
</template>
