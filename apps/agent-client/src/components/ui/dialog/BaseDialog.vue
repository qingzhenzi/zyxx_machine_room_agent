<script setup lang="ts">
import { X } from 'lucide-vue-next'
import { computed, toRef, useId, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'

import { useDialogFocus } from '@/composables/useDialogFocus'

type DialogSize = 'sm' | 'md' | 'lg' | 'xl' | 'full'
type DialogTone = 'default' | 'danger' | 'warning' | 'success' | 'info'

const { t } = useI18n()
const props = withDefaults(
  defineProps<{
    open: boolean
    title?: string
    description?: string
    size?: DialogSize
    width?: string
    tone?: DialogTone
    role?: 'dialog' | 'alertdialog'
    persistent?: boolean
    showClose?: boolean
    closeOnOverlay?: boolean
    contentClass?: string
  }>(),
  {
    title: '',
    description: '',
    size: 'md',
    width: '',
    tone: 'default',
    role: 'dialog',
    persistent: false,
    showClose: true,
    closeOnOverlay: true,
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

const sizeMap: Record<DialogSize, string> = {
  sm: '420px',
  md: '540px',
  lg: '720px',
  xl: '920px',
  full: 'min(1120px, calc(100vw - 48px))',
}

const maxWidth = computed(() => props.width || sizeMap[props.size])
const labelledBy = computed(() => (props.title ? titleId : undefined))
const describedBy = computed(() => (props.description ? descriptionId : undefined))

function requestClose() {
  if (props.persistent) return
  emit('close')
}

function onOverlay() {
  if (props.persistent || !props.closeOnOverlay) return
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
      name="dialog"
      @after-enter="emit('opened')"
      @after-leave="emit('closed')"
    >
      <div
        v-if="open"
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/45 p-6 backdrop-blur-[2px]"
        @mousedown.self="onOverlay"
      >
        <div
          ref="panel"
          class="panel dialog-panel flex max-h-[86vh] w-full flex-col overflow-hidden shadow-[var(--shadow-float)] outline-none"
          :class="tone === 'default' ? '' : `dialog-tone-${tone}`"
          :style="{ maxWidth }"
          :role="role"
          aria-modal="true"
          :aria-labelledby="labelledBy"
          :aria-describedby="describedBy"
          tabindex="-1"
        >
          <header
            v-if="title || description || $slots.header || $slots.icon"
            class="flex items-start justify-between gap-4 border-b border-line px-5 py-4"
          >
            <div class="flex min-w-0 items-start gap-3">
              <div v-if="$slots.icon" class="mt-0.5 shrink-0">
                <slot name="icon" />
              </div>
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
            </div>
            <button
              v-if="showClose && !persistent"
              type="button"
              class="icon-btn -mr-1 -mt-1"
              :aria-label="t('关闭')"
              @click="requestClose"
            >
              <X :size="15" />
            </button>
          </header>

          <div
            v-if="$slots.default"
            class="min-h-0 flex-1 overflow-y-auto px-5 py-4"
            :class="contentClass"
          >
            <slot />
          </div>

          <footer
            v-if="$slots.footer"
            class="flex items-center justify-end gap-2 border-t border-line bg-surface-2 px-5 py-3"
          >
            <slot name="footer" />
          </footer>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>
