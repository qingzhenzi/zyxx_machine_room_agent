<script setup lang="ts">
import { useI18n } from 'vue-i18n'
export interface ChipOption {
  value: string
  label: string
  count?: number
}

const { t } = useI18n()

withDefaults(
  defineProps<{
    modelValue: string
    options: ChipOption[]
    ariaLabel?: string
  }>(),
  { ariaLabel: 'common.filter' },
)

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <div class="flex items-center gap-0.5" role="group" :aria-label="t(ariaLabel)">
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      class="chip"
      :class="{ 'chip-active': modelValue === option.value }"
      :aria-pressed="modelValue === option.value"
      @click="emit('update:modelValue', option.value)"
    >
      {{ option.label }}
      <span v-if="option.count !== undefined" class="mono text-[9px] text-faint">
        {{ option.count }}
      </span>
    </button>
  </div>
</template>
