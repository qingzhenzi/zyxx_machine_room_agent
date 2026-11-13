<script setup lang="ts">
import type { Component } from 'vue'
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

export interface PopoverMenuItem {
  id: string
  label: string
  disabled?: boolean
  danger?: boolean
  dividerBefore?: boolean
  icon?: Component
}

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    open: boolean
    items: PopoverMenuItem[]
    width?: string
    ariaLabel?: string
  }>(),
  {
    width: '184px',
    ariaLabel: '菜单',
  },
)

const emit = defineEmits<{
  select: [id: string]
  close: []
}>()

const panelRef = ref<HTMLElement | null>(null)
let previousFocus: HTMLElement | null = null

function choose(item: PopoverMenuItem) {
  if (item.disabled) return
  emit('select', item.id)
  emit('close')
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

watch(
  () => props.open,
  async (open) => {
    if (open) {
      previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
      window.addEventListener('keydown', onKeydown)
      await nextTick()
      panelRef.value?.querySelector<HTMLElement>('button:not([disabled])')?.focus()
    } else {
      window.removeEventListener('keydown', onKeydown)
      previousFocus?.focus?.()
      previousFocus = null
    }
  },
)

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
})
</script>

<template>
  <div class="absolute z-30" :style="{ width }">
    <div v-if="open" class="fixed inset-0 z-0" @mousedown="emit('close')" />

    <Transition name="menu">
      <div
        v-if="open"
        ref="panelRef"
        class="panel relative z-10 w-full overflow-hidden p-1 shadow-[var(--shadow-float)]"
        role="menu"
        :aria-label="t(ariaLabel)"
      >
        <template v-for="item in items" :key="item.id">
          <div v-if="item.dividerBefore" class="my-1 h-px bg-line" />
          <button
            type="button"
            role="menuitem"
            class="flex h-7 w-full items-center gap-2 rounded px-2 text-left text-[11.5px] transition-colors"
            :class="
              item.danger
                ? 'text-danger hover:bg-danger-soft'
                : 'text-ink-soft hover:bg-surface-2'
            "
            :disabled="item.disabled"
            @click="choose(item)"
          >
            <component :is="item.icon" v-if="item.icon" :size="12" class="shrink-0" />
            {{ item.label }}
          </button>
        </template>
      </div>
    </Transition>
  </div>
</template>
