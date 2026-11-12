<script setup lang="ts">
import { AlertTriangle } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()

withDefaults(
  defineProps<{
    title?: string
    message: string
    retryLabel?: string
  }>(),
  { title: '加载失败', retryLabel: '重试' },
)

const emit = defineEmits<{ retry: [] }>()
</script>

<template>
  <div class="flex flex-1 flex-col items-center justify-center gap-2 px-6 py-10 text-center" role="alert">
    <div
      class="flex h-10 w-10 items-center justify-center rounded-full border border-danger/30 bg-danger-soft text-danger"
    >
      <AlertTriangle :size="17" />
    </div>
    <p class="text-[11.5px] font-medium text-ink">{{ t(title) }}</p>
    <p class="max-w-[380px] text-[10px] leading-4 text-muted">{{ message }}</p>
    <button type="button" class="btn btn-sm mt-1" @click="emit('retry')">
      {{ t(retryLabel) }}
    </button>
  </div>
</template>
