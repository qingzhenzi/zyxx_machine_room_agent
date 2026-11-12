<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { ArrowRight, ListChecks } from 'lucide-vue-next'

import type { ItemInput } from '@/api'
import ItemFormDialog from '@/components/items/ItemFormDialog.vue'
import EmptyState from '@/components/ui/EmptyState.vue'
import ErrorState from '@/components/ui/ErrorState.vue'
import LoadingState from '@/components/ui/LoadingState.vue'
import MetricTile from '@/components/ui/MetricTile.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { relativeFromNow } from '@/lib/format'
import { itemStatusTone } from '@/lib/status'
import { modKey } from '@/lib/platform'
import { useItemsStore } from '@/stores/items'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const router = useRouter()
const items = useItemsStore()
const ui = useUiStore()

const formOpen = ref(false)
const busy = ref(false)

const recent = computed(() =>
  [...items.items]
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
    .slice(0, 5),
)

async function save(input: ItemInput) {
  busy.value = true
  try {
    await items.create(input)
    formOpen.value = false
    ui.pushToast({ kind: 'success', title: t('items.created'), detail: input.title })
  } catch (error) {
    ui.pushToast({
      kind: 'error',
      title: t('items.saveFailed'),
      detail: error instanceof Error ? error.message : undefined,
    })
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  if (items.status === 'idle') void items.load()
})
</script>

<template>
  <div class="flex h-full flex-col overflow-y-auto">
    <header class="flex items-start justify-between gap-6 px-5 pb-3.5 pt-4">
      <div>
        <h1 class="text-[18px] font-semibold tracking-[-0.02em] text-ink">{{ t('home.title') }}</h1>
        <p class="mt-1 text-[11px] text-muted">{{ t('home.subtitle') }}</p>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <button type="button" class="btn btn-primary" @click="formOpen = true">
          <ListChecks :size="12" />
          {{ t('home.newItem') }}
        </button>
      </div>
    </header>

    <LoadingState v-if="items.status === 'loading'" :label="t('common.loading')" />
    <ErrorState
      v-else-if="items.status === 'error'"
      :title="t('common.error')"
      :message="items.error ?? t('items.loadFailed')"
      :retry-label="t('common.retry')"
      @retry="items.load()"
    />
    <template v-else>
      <section class="grid grid-cols-2 gap-3 px-5 xl:grid-cols-4">
        <MetricTile :label="t('home.metricTotal')" :value="String(items.counts.total)" />
        <MetricTile
          :label="t('home.metricActive')"
          :value="String(items.counts.active)"
          tone="info"
        />
        <MetricTile
          :label="t('home.metricDone')"
          :value="String(items.counts.done)"
          tone="success"
        />
        <MetricTile
          :label="t('home.metricRate')"
          :value="`${items.completionRate}%`"
          tone="success"
          :note="`${items.counts.todo} ${t('items.status.todo')}`"
        />
      </section>

      <section class="mt-3 px-5">
        <article class="panel">
          <header class="flex items-center justify-between border-b border-line px-4 py-2.5">
            <h2 class="text-[12px] font-semibold text-ink">{{ t('home.recent') }}</h2>
            <button
              type="button"
              class="flex items-center gap-1 text-[10.5px] text-muted transition-colors hover:text-ink"
              @click="router.push('/items')"
            >
              {{ t('home.viewAll') }}
              <ArrowRight :size="11" />
            </button>
          </header>

          <EmptyState
            v-if="recent.length === 0"
            :title="t('home.emptyTitle')"
            :description="t('home.emptyDescription')"
            :action-label="t('home.newItem')"
            @action="formOpen = true"
          />

          <ul v-else class="divide-y divide-line">
            <li
              v-for="item in recent"
              :key="item.id"
              class="flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-surface-2"
            >
              <StatusBadge
                :label="t(`items.status.${item.status}`)"
                :tone="itemStatusTone[item.status]"
                :dot="false"
              />
              <div class="min-w-0 flex-1">
                <p class="truncate text-[11.5px] font-medium text-ink">{{ item.title }}</p>
                <p class="truncate text-[10px] text-faint">{{ item.description }}</p>
              </div>
              <span class="mono shrink-0 text-[9.5px] text-faint">
                {{ relativeFromNow(item.updated_at) }}
              </span>
            </li>
          </ul>
        </article>

        <p class="mt-3 text-[10px] text-faint">
          {{ t('home.tip', { key: `${modKey} K` }) }}
        </p>
      </section>
    </template>

    <ItemFormDialog
      :open="formOpen"
      :busy="busy"
      @submit="save"
      @cancel="formOpen = false"
    />
  </div>
</template>
