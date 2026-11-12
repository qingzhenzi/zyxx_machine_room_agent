<script setup lang="ts">
import type { Component } from 'vue'

withDefaults(
  defineProps<{
    title: string
    subtitle: string
    description: string
    icon: Component
    iconClass?: string
  }>(),
  { iconClass: 'border-line bg-surface-2 text-muted' },
)
</script>

<template>
  <article class="panel flex flex-col p-4">
    <div class="flex items-center gap-2">
      <div
        class="flex h-8 w-8 items-center justify-center rounded border"
        :class="iconClass"
      >
        <component :is="icon" :size="15" />
      </div>
      <div>
        <h2 class="text-[12px] font-semibold text-ink">{{ title }}</h2>
        <p class="text-[9.5px] text-faint">{{ subtitle }}</p>
      </div>
    </div>
    <p class="mt-3 flex-1 text-[10.5px] leading-5 text-muted">{{ description }}</p>
    <div class="mt-3 flex flex-wrap items-center gap-2">
      <slot />
    </div>
  </article>
</template>
