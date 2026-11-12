<script setup lang="ts">
import { Loader2 } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()

withDefaults(
  defineProps<{
    label?: string
    compact?: boolean
  }>(),
  { label: 'common.loading', compact: false },
)
</script>

<template>
  <div class="flex flex-1 flex-col items-center justify-center gap-3 px-6 py-10" role="status">
    <Loader2 :size="20" class="animate-spin-slow text-accent" />
    <p class="text-[11px] text-muted">{{ t(label) }}</p>
    <div v-if="!compact" class="mt-1 w-full max-w-[360px] space-y-2">
      <div
        v-for="row in 3"
        :key="row"
        class="h-8 animate-pulse rounded border border-line bg-surface-2/60"
        :style="{ animationDelay: `${row * 90}ms` }"
      />
    </div>
  </div>
</template>
