<script setup lang="ts">
import { Loader2 } from 'lucide-vue-next'
import { useId } from 'vue'
import { useI18n } from 'vue-i18n'

import BaseDialog from '@/components/ui/dialog/BaseDialog.vue'

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    open: boolean
    title: string
    description?: string
    width?: string
    submitLabel?: string
    cancelLabel?: string
    busy?: boolean
    submitDisabled?: boolean
    persistent?: boolean
  }>(),
  {
    description: '',
    width: '560px',
    submitLabel: '保存',
    cancelLabel: '取消',
    busy: false,
    submitDisabled: false,
    persistent: false,
  },
)

const emit = defineEmits<{
  submit: []
  cancel: []
}>()

const formId = useId()

function submit() {
  if (props.busy || props.submitDisabled) return
  emit('submit')
}
</script>

<template>
  <BaseDialog
    :open="open"
    :title="title"
    :description="description"
    :width="width"
    :persistent="persistent || busy"
    @close="emit('cancel')"
  >
    <form :id="formId" @submit.prevent="submit">
      <slot />
    </form>

    <template #footer>
      <button type="button" class="btn" :disabled="busy" @click="emit('cancel')">
        {{ t(cancelLabel) }}
      </button>
      <button
        type="submit"
        class="btn btn-primary"
        :form="formId"
        :disabled="busy || submitDisabled"
      >
        <Loader2 v-if="busy" :size="12" class="animate-spin-slow" />
        {{ t(submitLabel) }}
      </button>
    </template>
  </BaseDialog>
</template>
