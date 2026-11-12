<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()

withDefaults(
  defineProps<{
    modelValue: string
    options: Array<{ value: string; label: string }>
    ariaLabel?: string
  }>(),
  { ariaLabel: 'common.segmented' },
)

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <div
    class="flex rounded-[5px] border border-line bg-surface p-0.5"
    role="group"
    :aria-label="t(ariaLabel)"
  >
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      class="h-[26px] rounded-[3px] px-2.5 text-[11px] transition-colors"
      :class="
        modelValue === option.value
          ? 'bg-surface-3 font-medium text-ink'
          : 'text-muted hover:text-ink'
      "
      :aria-pressed="modelValue === option.value"
      @click="emit('update:modelValue', option.value)"
    >
      {{ option.label }}
    </button>
  </div>
</template>
