<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, CheckCircle2, Info, Loader2 } from 'lucide-vue-next'

import BaseDialog from '@/components/ui/dialog/BaseDialog.vue'

type ConfirmTone = 'danger' | 'warning' | 'success' | 'info'

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    open: boolean
    title: string
    description?: string
    tone?: ConfirmTone
    confirmLabel?: string
    cancelLabel?: string
    loading?: boolean
    persistent?: boolean
  }>(),
  {
    description: '',
    tone: 'danger',
    confirmLabel: 'common.confirm',
    cancelLabel: 'common.cancel',
    loading: false,
    persistent: false,
  },
)

const emit = defineEmits<{
  confirm: []
  cancel: []
}>()

const meta = computed(() => {
  if (props.tone === 'success') {
    return { icon: CheckCircle2, iconClass: 'border-success/30 bg-success-soft text-success' }
  }
  if (props.tone === 'info') {
    return { icon: Info, iconClass: 'border-info/30 bg-info-soft text-info' }
  }
  if (props.tone === 'warning') {
    return { icon: AlertTriangle, iconClass: 'border-warning/30 bg-warning-soft text-warning' }
  }
  return { icon: AlertTriangle, iconClass: 'border-danger/30 bg-danger-soft text-danger' }
})
</script>

<template>
  <BaseDialog
    :open="open"
    :title="title"
    :description="description"
    :tone="tone"
    :persistent="persistent || loading"
    :show-close="false"
    role="alertdialog"
    width="440px"
    @close="emit('cancel')"
  >
    <template #icon>
      <span
        class="flex h-9 w-9 items-center justify-center rounded-full border"
        :class="meta.iconClass"
      >
        <component :is="meta.icon" :size="16" />
      </span>
    </template>

    <template #footer>
      <button type="button" class="btn" :disabled="loading" @click="emit('cancel')">
        {{ t(cancelLabel) }}
      </button>
      <button
        type="button"
        class="btn"
        :class="tone === 'danger' ? 'btn-danger' : 'btn-primary'"
        :disabled="loading"
        @click="emit('confirm')"
      >
        <Loader2 v-if="loading" :size="12" class="animate-spin-slow" />
        {{ t(confirmLabel) }}
      </button>
    </template>
  </BaseDialog>
</template>
