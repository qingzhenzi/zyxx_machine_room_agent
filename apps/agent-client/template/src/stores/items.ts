import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import { api, apiErrorMessage, type ItemDto, type ItemInput, type ItemStatus } from '@/api'

export type ItemFilter = 'all' | ItemStatus

export const useItemsStore = defineStore('items', () => {
  const items = ref<ItemDto[]>([])
  const status = ref<'idle' | 'loading' | 'ready' | 'error'>('idle')
  const error = ref<string | null>(null)
  const filter = ref<ItemFilter>('all')
  const keyword = ref('')

  const visibleItems = computed(() => {
    const term = keyword.value.trim().toLowerCase()
    return items.value
      .filter((item) => (filter.value === 'all' ? true : item.status === filter.value))
      .filter((item) => (term ? item.title.toLowerCase().includes(term) : true))
  })

  const counts = computed(() => ({
    total: items.value.length,
    active: items.value.filter((item) => item.status === 'doing').length,
    done: items.value.filter((item) => item.status === 'done').length,
    todo: items.value.filter((item) => item.status === 'todo').length,
  }))

  const completionRate = computed(() =>
    counts.value.total === 0 ? 0 : Math.round((counts.value.done / counts.value.total) * 100),
  )

  async function load() {
    status.value = status.value === 'ready' ? 'ready' : 'loading'
    error.value = null
    try {
      // The server accepts `status`/`keyword` params; this demo filters in
      // memory so switching filters never blocks the UI.
      items.value = await api.items.list()
      status.value = 'ready'
    } catch (caught) {
      error.value = apiErrorMessage(caught)
      status.value = 'error'
    }
  }

  async function create(input: ItemInput) {
    const item = await api.items.create(input)
    items.value = [item, ...items.value]
    return item
  }

  async function update(id: string, input: Partial<ItemInput>) {
    const updated = await api.items.update(id, input)
    items.value = items.value.map((item) => (item.id === id ? updated : item))
    return updated
  }

  async function remove(id: string) {
    await api.items.remove(id)
    items.value = items.value.filter((item) => item.id !== id)
  }

  function setFilter(next: ItemFilter) {
    filter.value = next
  }

  function setKeyword(next: string) {
    keyword.value = next
  }

  return {
    items,
    status,
    error,
    filter,
    keyword,
    visibleItems,
    counts,
    completionRate,
    load,
    create,
    update,
    remove,
    setFilter,
    setKeyword,
  }
})
