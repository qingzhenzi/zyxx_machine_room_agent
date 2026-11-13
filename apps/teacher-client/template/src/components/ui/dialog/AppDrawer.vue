<script setup lang="ts">
import { X } from 'lucide-vue-next'
import { computed, toRef, useId, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'

import { useDialogFocus } from '@/composables/useDialogFocus'

const { t } = useI18n()
const props = withDefaults(
  defineProps<{
    open: boolean
    title?: string
    description?: string
    side?: 'left' | 'right'
    width?: string
    persistent?: boolean
    showClose?: boolean
    contentClass?: string
  }>(),
  {
    title: '',
    description: '',
    side: 'right',
    width: '440px',
    persistent: false,
    showClose: true,
    contentClass: '',
  },
)

const emit = defineEmits<{
  close: []
  opened: []
  closed: []
}>()

const panelRef = useTemplateRef<HTMLElement>('panel')
const titleId = useId()
const descriptionId = useId()

const labelledBy = computed(() => (props.title ? titleId : undefined))
const describedBy = computed(() => (props.description ? descriptionId : undefined))

function requestClose() {
  if (props.persistent) return
  emit('close')
}

useDialogFocus({
  open: toRef(props, 'open'),
  panel: panelRef,
  onRequestClose: requestClose,
  closeOnEscape: () => !props.persistent,
})
</script>

<template>
  <Teleport to="body">
    <Transition
      name="drawer"
      @after-enter="emit('opened')"
      @after-leave="emit('closed')"
    >
      <div
        v-if="open"
        class="fixed inset-0 z-50 bg-black/35 backdrop-blur-[1px]"
        @mousedown.self="requestClose"
      >
        <aside
          ref="panel"
          class="panel drawer-panel absolute inset-y-0 flex w-full flex-col overflow-hidden shadow-[var(--shadow-float)] outline-none"
          :class="[side === 'right' ? 'right-0 drawer-panel-right' : 'left-0 drawer-panel-left']"
          :style="{ maxWidth: width }"
          role="dialog"
          aria-modal="true"
          :aria-labelledby="labelledBy"
          :aria-describedby="describedBy"
          tabindex="-1"
        >
          <header
            v-if="title || description || $slots.header"
            class="flex shrink-0 items-start justify-between gap-4 border-b border-line px-5 py-4"
          >
            <div class="min-w-0">
              <slot name="header">
                <h2
                  v-if="title"
                  :id="titleId"
                  class="text-[14px] font-semibold tracking-[-0.01em] text-ink"
                >
                  {{ title }}
                </h2>
                <p
                  v-if="description"
                  :id="descriptionId"
                  class="mt-1 text-[11.5px] leading-5 text-muted"
                >
                  {{ description }}
                </p>
              </slot>
            </div>
            <button
              v-if="showClose && !persistent"
              type="button"
              class="icon-btn -mr-1 -mt-1"
              :aria-label="t('common.close')"
              @click="requestClose"
            >
              <X :size="15" />
            </button>
          </header>

          <div
            class="min-h-0 flex-1 overflow-y-auto px-5 py-4"
            :class="contentClass"
          >
            <slot />
          </div>

          <footer
            v-if="$slots.footer"
            class="flex shrink-0 items-center justify-end gap-2 border-t border-line bg-surface-2 px-5 py-3"
          >
            <slot name="footer" />
          </footer>
        </aside>
      </div>
    </Transition>
  </Teleport>
</template>
