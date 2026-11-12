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
    <!-- `left` must stay explicit: an `absolute` child without it is placed by the
         button's default text-align (centered), which pushes the knob off the track. -->
    <span
      class="absolute left-[2px] top-[2px] h-[12px] w-[12px] rounded-full transition-transform"
      :class="modelValue ? 'translate-x-[14px] bg-accent-contrast' : 'translate-x-0 bg-muted'"
    />
  </button>
</template>
