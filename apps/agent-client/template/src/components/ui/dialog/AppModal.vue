<script setup lang="ts">
import BaseDialog from '@/components/ui/dialog/BaseDialog.vue'

type DialogTone = 'default' | 'danger' | 'warning' | 'success' | 'info'

withDefaults(
  defineProps<{
    open: boolean
    title: string
    description?: string
    width?: string
    tone?: DialogTone
    persistent?: boolean
    showClose?: boolean
  }>(),
  {
    description: '',
    width: '540px',
    tone: 'default',
    persistent: false,
    showClose: true,
  },
)

const emit = defineEmits<{ close: [] }>()
</script>

<template>
  <BaseDialog
    :open="open"
    :title="title"
    :description="description"
    :width="width"
    :tone="tone"
    :persistent="persistent"
    :show-close="showClose"
    @close="emit('close')"
  >
    <template v-if="$slots.icon" #icon>
      <slot name="icon" />
    </template>
    <template v-if="$slots.footer" #footer>
      <slot name="footer" />
    </template>
    <slot />
  </BaseDialog>
</template>
