<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

import type { ItemDto, ItemInput, ItemStatus } from '@/api'
import FormDialog from '@/components/ui/dialog/FormDialog.vue'

const props = withDefaults(
  defineProps<{
    open: boolean
    item?: ItemDto | null
    busy?: boolean
  }>(),
  { item: null, busy: false },
)

const emit = defineEmits<{
  submit: [input: ItemInput]
  cancel: []
}>()

const { t } = useI18n()

const form = ref<{ title: string; description: string; status: ItemStatus }>({
  title: '',
  description: '',
  status: 'todo',
})

const dialogTitle = computed(() =>
  props.item ? t('items.formEditTitle') : t('items.formCreateTitle'),
)

const statusOptions = computed<Array<{ value: ItemStatus; label: string }>>(() => [
  { value: 'todo', label: t('items.status.todo') },
  { value: 'doing', label: t('items.status.doing') },
  { value: 'done', label: t('items.status.done') },
])

watch(
  () => props.open,
  (open) => {
    if (!open) return
    form.value = props.item
      ? {
          title: props.item.title,
          description: props.item.description,
          status: props.item.status,
        }
      : { title: '', description: '', status: 'todo' }
  },
)

function submit() {
  const title = form.value.title.trim()
  if (!title) return
  emit('submit', {
    title,
    description: form.value.description.trim(),
    status: form.value.status,
  })
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="dialogTitle"
    :submit-label="t('common.save')"
    :cancel-label="t('common.cancel')"
    :busy="busy"
    :submit-disabled="!form.title.trim()"
    @submit="submit"
    @cancel="emit('cancel')"
  >
    <div class="space-y-3">
      <label class="block">
        <span class="mb-1.5 block text-[10.5px] text-muted">{{ t('items.name') }}</span>
        <div class="field">
          <input v-model="form.title" :placeholder="t('items.namePlaceholder')" autofocus />
        </div>
      </label>

      <label class="block">
        <span class="mb-1.5 block text-[10.5px] text-muted">{{ t('items.description') }}</span>
        <div class="field">
          <input v-model="form.description" :placeholder="t('items.descriptionPlaceholder')" />
        </div>
      </label>

      <label class="block">
        <span class="mb-1.5 block text-[10.5px] text-muted">{{ t('common.status') }}</span>
        <div class="field">
          <select v-model="form.status">
            <option v-for="option in statusOptions" :key="option.value" :value="option.value">
              {{ option.label }}
            </option>
          </select>
        </div>
      </label>
    </div>
  </FormDialog>
</template>
