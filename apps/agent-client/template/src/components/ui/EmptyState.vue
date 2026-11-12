<script setup lang="ts">
import type { Component } from 'vue'

withDefaults(
  defineProps<{
    title: string
    description?: string
    icon?: Component
    actionLabel?: string
  }>(),
  { description: '', actionLabel: '' },
)

const emit = defineEmits<{ action: [] }>()
</script>

<template>
  <div class="flex flex-col items-center justify-center gap-2 px-6 py-8 text-center">
    <div
      v-if="icon"
      class="flex h-9 w-9 items-center justify-center rounded-full border border-line bg-surface-2 text-faint"
    >
      <component :is="icon" :size="16" />
    </div>
    <p class="text-[11.5px] font-medium text-ink">{{ title }}</p>
    <p v-if="description" class="max-w-[340px] text-[10px] leading-4 text-muted">
      {{ description }}
    </p>
    <button v-if="actionLabel" type="button" class="btn btn-sm mt-1" @click="emit('action')">
      {{ actionLabel }}
    </button>
    <slot />
  </div>
</template>
