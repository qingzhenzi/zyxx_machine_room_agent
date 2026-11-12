<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { Pencil, Plus, Search, Trash2 } from 'lucide-vue-next'

import type { ItemDto, ItemInput } from '@/api'
import ItemFormDialog from '@/components/items/ItemFormDialog.vue'
import ConfirmDialog from '@/components/ui/dialog/ConfirmDialog.vue'
import EmptyState from '@/components/ui/EmptyState.vue'
import ErrorState from '@/components/ui/ErrorState.vue'
import FilterChips from '@/components/ui/FilterChips.vue'
import LoadingState from '@/components/ui/LoadingState.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { relativeFromNow } from '@/lib/format'
import { itemStatusTone } from '@/lib/status'
import { useItemsStore, type ItemFilter } from '@/stores/items'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const items = useItemsStore()
const ui = useUiStore()

const formOpen = ref(false)
const editing = ref<ItemDto | null>(null)
const saving = ref(false)
const deleteTarget = ref<ItemDto | null>(null)
const deleting = ref(false)

const filterValue = computed({
  get: () => items.filter,
  set: (value: string) => items.setFilter(value as ItemFilter),
})

const keyword = computed({
  get: () => items.keyword,
  set: (value: string) => items.setKeyword(value),
})

const chipOptions = computed(() => [
  { value: 'all', label: t('items.filter.all'), count: items.counts.total },
  { value: 'todo', label: t('items.filter.todo'), count: items.counts.todo },
  { value: 'doing', label: t('items.filter.doing'), count: items.counts.active },
  { value: 'done', label: t('items.filter.done'), count: items.counts.done },
])

function openCreate() {
  editing.value = null
  formOpen.value = true
}

function openEdit(item: ItemDto) {
  editing.value = item
  formOpen.value = true
}

async function save(input: ItemInput) {
  saving.value = true
  try {
    if (editing.value) {
      await items.update(editing.value.id, input)
      ui.pushToast({ kind: 'success', title: t('items.updated'), detail: input.title })
    } else {
      await items.create(input)
      ui.pushToast({ kind: 'success', title: t('items.created'), detail: input.title })
    }
    formOpen.value = false
    editing.value = null
  } catch (error) {
    ui.pushToast({
      kind: 'error',
      title: t('items.saveFailed'),
      detail: error instanceof Error ? error.message : undefined,
    })
  } finally {
    saving.value = false
  }
}

async function confirmDelete() {
  const target = deleteTarget.value
  if (!target) return

  deleting.value = true
  try {
    await items.remove(target.id)
    ui.pushToast({ kind: 'success', title: t('items.deleted'), detail: target.title })
    deleteTarget.value = null
  } catch (error) {
    ui.pushToast({
      kind: 'error',
      title: t('common.error'),
      detail: error instanceof Error ? error.message : undefined,
    })
  } finally {
    deleting.value = false
  }
}

watch(
  () => route.query.new,
  (value) => {
    if (value !== '1') return
    openCreate()
    void router.replace({ path: '/items' })
  },
  { immediate: true },
)

onMounted(() => {
  if (items.status === 'idle') void items.load()
})
</script>

<template>
  <div class="flex h-full flex-col overflow-hidden">
    <header class="flex items-start justify-between gap-6 px-5 pb-3.5 pt-4">
      <div>
        <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">{{ t('items.title') }}</h1>
        <p class="mt-1 text-[11px] text-muted">{{ t('items.subtitle') }}</p>
      </div>
      <button type="button" class="btn btn-primary shrink-0" @click="openCreate">
        <Plus :size="12" />
        {{ t('items.new') }}
      </button>
    </header>

    <div class="flex items-center gap-3 px-5 pb-3">
      <FilterChips v-model="filterValue" :options="chipOptions" :aria-label="t('common.filter')" />
      <div class="field ml-auto w-[240px]">
        <Search :size="12" class="shrink-0 text-faint" />
        <input v-model="keyword" :placeholder="t('items.searchPlaceholder')" />
      </div>
    </div>

    <LoadingState v-if="items.status === 'loading'" :label="t('common.loading')" />
    <ErrorState
      v-else-if="items.status === 'error'"
      :title="t('common.error')"
      :message="items.error ?? t('items.loadFailed')"
      :retry-label="t('common.retry')"
      @retry="items.load()"
    />
    <EmptyState
      v-else-if="items.visibleItems.length === 0"
      :title="t('items.emptyTitle')"
      :description="t('items.emptyDescription')"
      :action-label="t('items.new')"
      @action="openCreate"
    />

    <section v-else class="min-h-0 flex-1 overflow-y-auto px-5 pb-4">
      <article class="panel overflow-hidden">
        <div
          class="grid grid-cols-[minmax(0,1fr)_130px_120px_64px] items-center gap-3 border-b border-line bg-surface-2 px-4 py-2 text-[10px] font-medium uppercase tracking-[0.06em] text-faint"
        >
          <span>{{ t('items.column.title') }}</span>
          <span>{{ t('items.column.status') }}</span>
          <span>{{ t('items.column.updated') }}</span>
          <span class="text-right">{{ t('common.actions') }}</span>
        </div>

        <div
          v-for="item in items.visibleItems"
          :key="item.id"
          class="grid grid-cols-[minmax(0,1fr)_130px_120px_64px] items-center gap-3 border-b border-line px-4 py-2.5 last:border-b-0 transition-colors hover:bg-surface-2"
        >
          <div class="min-w-0">
            <p class="truncate text-[11.5px] font-medium text-ink">{{ item.title }}</p>
            <p v-if="item.description" class="truncate text-[10px] text-faint">
              {{ item.description }}
            </p>
          </div>
          <StatusBadge
            class="justify-self-start"
            :label="t(`items.status.${item.status}`)"
            :tone="itemStatusTone[item.status]"
          />
          <span class="mono text-[9.5px] text-faint">{{ relativeFromNow(item.updated_at) }}</span>
          <div class="flex items-center justify-end gap-1">
            <button
              type="button"
              class="icon-btn"
              :title="t('common.edit')"
              :aria-label="t('common.edit')"
              @click="openEdit(item)"
            >
              <Pencil :size="12" />
            </button>
            <button
              type="button"
              class="icon-btn text-danger"
              :title="t('common.delete')"
              :aria-label="t('common.delete')"
              @click="deleteTarget = item"
            >
              <Trash2 :size="12" />
            </button>
          </div>
        </div>
      </article>
    </section>

    <ItemFormDialog
      :open="formOpen"
      :item="editing"
      :busy="saving"
      @submit="save"
      @cancel="formOpen = false"
    />

    <ConfirmDialog
      :open="deleteTarget !== null"
      :title="t('items.deleteTitle')"
      :description="t('items.deleteDescription')"
      :confirm-label="t('common.delete')"
      :cancel-label="t('common.cancel')"
      :loading="deleting"
      tone="danger"
      @confirm="confirmDelete"
      @cancel="deleteTarget = null"
    />
  </div>
</template>
