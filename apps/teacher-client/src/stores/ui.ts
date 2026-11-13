import { ref } from 'vue'
import { defineStore } from 'pinia'

export type ToastKind = 'success' | 'error' | 'warning' | 'info'

export interface Toast {
  id: number
  kind: ToastKind
  title: string
  detail?: string
}

export interface ToastInput {
  kind?: ToastKind
  title: string
  detail?: string
}

let toastId = 0
const dismissTimers = new Map<number, ReturnType<typeof setTimeout>>()

export const useUiStore = defineStore('ui', () => {
  const toasts = ref<Toast[]>([])

  function dismissToast(id: number) {
    const timer = dismissTimers.get(id)
    if (timer) clearTimeout(timer)
    dismissTimers.delete(id)
    toasts.value = toasts.value.filter((toast) => toast.id !== id)
  }

  function pushToast(input: ToastInput) {
    const id = ++toastId
    toasts.value = [
      ...toasts.value.slice(-3),
      {
        id,
        kind: input.kind ?? 'info',
        title: input.title,
        detail: input.detail,
      },
    ]
    dismissTimers.set(
      id,
      setTimeout(() => dismissToast(id), 4200),
    )
    return id
  }

  return {
    toasts,
    pushToast,
    dismissToast,
  }
})
