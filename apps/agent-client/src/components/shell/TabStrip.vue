<script setup lang="ts">
import { ChevronDown, X } from 'lucide-vue-next'
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'

import PopoverMenu, {
  type PopoverMenuItem,
} from '@/components/ui/dialog/PopoverMenu.vue'
import { appIcon } from '@/lib/icons'
import { useTabsStore } from '@/stores/tabs'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const tabs = useTabsStore()
const menuOpen = ref(false)

const activePath = computed(() => route.path)
const canCloseOthers = computed(() => tabs.tabs.length > 1)

const tabMenuItems = computed<PopoverMenuItem[]>(() => [
  { id: 'close-others', label: t('关闭其他标签页'), disabled: !canCloseOthers.value },
  { id: 'close-all', label: t('关闭全部标签页'), disabled: !canCloseOthers.value },
])

function selectTab(path: string) {
  if (path !== route.path) void router.push(path)
}

function closeTab(path: string) {
  const next = tabs.close(path)
  if (next) void router.push(next)
}

function handleMenuAction(id: string) {
  if (id === 'close-others') tabs.closeOthers(route.path)
  if (id === 'close-all') {
    tabs.closeAll()
    if (route.path !== '/') void router.push('/')
  }
}
</script>

<template>
  <div class="relative flex h-[35px] items-stretch border-b border-line bg-canvas">
    <div class="scrollbar-none flex min-w-0 flex-1 items-stretch overflow-x-auto">
      <div
        v-for="tab in tabs.tabs"
        :key="tab.path"
        class="group relative flex h-full min-w-[118px] max-w-[190px] items-center border-r border-line border-t-2 text-[11.5px] transition-colors"
        :class="
          activePath === tab.path
            ? 'border-t-accent bg-surface text-ink'
            : 'border-t-transparent text-muted hover:bg-surface-2 hover:text-ink-soft'
        "
        @auxclick.middle.prevent="closeTab(tab.path)"
      >
        <button
          type="button"
          class="flex h-full w-full items-center gap-2 pl-3 text-left"
          :class="tab.pinned ? 'pr-3' : 'pr-8'"
          @click="selectTab(tab.path)"
        >
          <component
            :is="appIcon(tab.icon)"
            :size="13"
            class="shrink-0"
            :class="activePath === tab.path ? 'text-accent' : 'text-faint'"
          />
          <span class="min-w-0 flex-1 truncate">{{ tab.title }}</span>
          <span
            v-if="tab.pinned"
            class="h-1 w-1 shrink-0 rounded-full bg-accent"
            :title="t('固定标签页')"
          />
        </button>
        <button
          v-if="!tab.pinned"
          type="button"
          class="absolute right-1.5 top-1/2 flex h-4 w-4 -translate-y-1/2 items-center justify-center rounded-sm text-faint transition-colors hover:bg-surface-3 hover:text-ink"
          :class="activePath === tab.path ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'"
          :aria-label="`${t('关闭')} ${tab.title}`"
          @click.stop="closeTab(tab.path)"
        >
          <X :size="11" />
        </button>
      </div>
    </div>

    <div class="flex shrink-0 items-center border-l border-line bg-canvas">
      <button
        type="button"
        class="flex h-full w-8 items-center justify-center text-faint transition-colors hover:bg-surface-2 hover:text-ink"
        :title="t('标签页操作')"
        :aria-label="t('标签页操作')"
        @click="menuOpen = !menuOpen"
      >
        <ChevronDown :size="13" />
      </button>
    </div>

    <PopoverMenu
      class="right-1 top-[34px]"
      :open="menuOpen"
      :items="tabMenuItems"
      :aria-label="t('标签页操作')"
      @select="handleMenuAction"
      @close="menuOpen = false"
    />
  </div>
</template>
