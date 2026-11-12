<script setup lang="ts">
import { AlertCircle, CheckCircle2, Info, X, XCircle } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

import type { ToastKind } from '@/stores/ui'
import { useUiStore } from '@/stores/ui'

const { t } = useI18n()
const ui = useUiStore()

function iconFor(kind: ToastKind) {
  if (kind === 'success') return CheckCircle2
  if (kind === 'error') return XCircle
  if (kind === 'warning') return AlertCircle
  return Info
}

function toneFor(kind: ToastKind) {
  if (kind === 'success') return 'text-success border-l-success'
  if (kind === 'error') return 'text-danger border-l-danger'
  if (kind === 'warning') return 'text-warning border-l-warning'
  return 'text-info border-l-info'
}
</script>

<template>
  <Teleport to="body">
    <TransitionGroup
      name="toast"
      tag="div"
      class="pointer-events-none fixed bottom-[32px] right-3 z-50 flex w-[318px] flex-col gap-2"
    >
      <div
        v-for="toast in ui.toasts"
        :key="toast.id"
        class="panel pointer-events-auto flex items-start gap-2.5 border-l-2 px-3 py-2.5 shadow-[var(--shadow-float)]"
        :class="toneFor(toast.kind)"
        role="status"
      >
        <component :is="iconFor(toast.kind)" :size="14" class="mt-0.5 shrink-0" />
        <div class="min-w-0 flex-1">
          <p class="text-[11.5px] font-medium text-ink">{{ toast.title }}</p>
          <p v-if="toast.detail" class="mt-0.5 text-[10.5px] leading-4 text-muted">
            {{ toast.detail }}
          </p>
        </div>
        <button
          type="button"
          class="shrink-0 rounded p-0.5 text-faint transition-colors hover:bg-surface-2 hover:text-ink"
          :aria-label="t('关闭通知')"
          @click="ui.dismissToast(toast.id)"
        >
          <X :size="11" />
        </button>
      </div>
    </TransitionGroup>
  </Teleport>
</template>
