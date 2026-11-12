<script setup lang="ts">
withDefaults(
  defineProps<{
    size?: number
    muted?: boolean
  }>(),
  { size: 22, muted: false },
)
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 32 32"
    fill="none"
    aria-hidden="true"
    class="shrink-0"
  >
    <rect
      width="32"
      height="32"
      rx="8"
      :fill="muted ? 'var(--surface-3)' : 'var(--accent)'"
    />
    <path
      d="M9 23V16.5M16 23V10.5M23 23v-4.5"
      :stroke="muted ? 'var(--muted)' : 'var(--accent-contrast)'"
      stroke-width="2.6"
      stroke-linecap="round"
    />
    <circle
      cx="23"
      cy="12.5"
      r="2"
      :fill="muted ? 'var(--muted)' : 'var(--accent-contrast)'"
    />
  </svg>
</template>
