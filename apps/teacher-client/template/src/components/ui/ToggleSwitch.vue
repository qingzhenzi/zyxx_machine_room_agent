<script setup lang="ts">
defineProps<{
  modelValue: boolean
  disabled?: boolean
  label?: string
}>()

const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="label"
    :disabled="disabled"
    class="relative h-[18px] w-[32px] shrink-0 rounded-full border transition-colors disabled:opacity-40"
    :class="modelValue ? 'border-accent bg-accent' : 'border-line-strong bg-surface-3'"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span
      class="absolute top-[2px] h-[12px] w-[12px] rounded-full transition-transform"
      :class="modelValue ? 'translate-x-[17px] bg-accent-contrast' : 'translate-x-[2px] bg-muted'"
    />
  </button>
</template>
