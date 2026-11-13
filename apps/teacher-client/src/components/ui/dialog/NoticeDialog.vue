<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, CheckCircle2, Info } from 'lucide-vue-next'

import BaseDialog from '@/components/ui/dialog/BaseDialog.vue'

type NoticeTone = 'success' | 'info' | 'warning'

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    open: boolean
    title: string
    description?: string
    tone?: NoticeTone
    okLabel?: string
  }>(),
  {
    description: '',
    tone: 'info',
    okLabel: '知道了',
  },
)

const emit = defineEmits<{ close: [] }>()

const meta = computed(() => {
  if (props.tone === 'success') {
    return { icon: CheckCircle2, iconClass: 'border-success/30 bg-success-soft text-success' }
  }
  if (props.tone === 'warning') {
    return { icon: AlertTriangle, iconClass: 'border-warning/30 bg-warning-soft text-warning' }
  }
  return { icon: Info, iconClass: 'border-info/30 bg-info-soft text-info' }
})
</script>

<template>
  <BaseDialog
    :open="open"
    :title="title"
    :description="description"
    :tone="tone"
    :show-close="false"
    width="440px"
    @close="emit('close')"
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
      <button type="button" class="btn btn-primary" @click="emit('close')">
        {{ t(okLabel) }}
      </button>
    </template>
  </BaseDialog>
</template>
