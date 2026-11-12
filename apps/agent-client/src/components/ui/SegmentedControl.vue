<script setup lang="ts">
import { useI18n } from 'vue-i18n'
const { t } = useI18n()

withDefaults(
  defineProps<{
    modelValue: string
    options: string[]
    ariaLabel?: string
  }>(),
  { ariaLabel: '分段选择' },
)

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <div
    class="flex h-[30px] rounded-[5px] border border-line bg-surface p-0.5"
    role="group"
    :aria-label="t(ariaLabel)"
  >
    <button
      v-for="option in options"
      :key="option"
      type="button"
      class="rounded-[3px] px-2.5 text-[11px] transition-colors"
      :class="
        modelValue === option
          ? 'bg-surface-3 font-medium text-ink'
          : 'text-muted hover:text-ink'
      "
      :aria-pressed="modelValue === option"
      @click="emit('update:modelValue', option)"
    >
      {{ option }}
    </button>
  </div>
</template>
