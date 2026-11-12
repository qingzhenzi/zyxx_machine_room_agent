import { nextTick, onBeforeUnmount, watch, type Ref } from 'vue'

interface DialogFocusOptions {
  open: Ref<boolean>
  panel: Ref<HTMLElement | null>
  onRequestClose: () => void
  closeOnEscape?: () => boolean
}

const focusableSelector = [
  'a[href]',
  'button:not([disabled])',
  'textarea:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

/**
 * Shared dialog behavior: body scroll lock, Escape, focus trap and focus
 * restoration. Used by the centered dialog and the side drawer.
 */
export function useDialogFocus({
  open,
  panel,
  onRequestClose,
  closeOnEscape,
}: DialogFocusOptions) {
  let previousFocus: HTMLElement | null = null
  let previousOverflow = ''
  let active = false

  function focusableElements(): HTMLElement[] {
    return panel.value ? Array.from(panel.value.querySelectorAll<HTMLElement>(focusableSelector)) : []
  }

  function handleKeydown(event: KeyboardEvent) {
    if (!active) return

    if (event.key === 'Escape' && (closeOnEscape?.() ?? true)) {
      event.preventDefault()
      onRequestClose()
      return
    }

    if (event.key !== 'Tab') return

    const focusables = focusableElements()

    if (focusables.length === 0) {
      event.preventDefault()
      panel.value?.focus()
      return
    }

    const first = focusables[0]!
    const last = focusables[focusables.length - 1]!
    const current = document.activeElement

    if (event.shiftKey && (current === first || current === panel.value)) {
      event.preventDefault()
      last.focus()
    } else if (!event.shiftKey && current === last) {
      event.preventDefault()
      first.focus()
    }
  }

  async function activate() {
    if (active) return

    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
    previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    active = true
    window.addEventListener('keydown', handleKeydown)

    await nextTick()
    panel.value?.focus()
  }

  function deactivate() {
    if (!active) return

    active = false
    window.removeEventListener('keydown', handleKeydown)
    document.body.style.overflow = previousOverflow
    previousFocus?.focus?.()
    previousFocus = null
  }

  watch(
    open,
    (isOpen) => {
      if (isOpen) void activate()
      else deactivate()
    },
    { immediate: true },
  )

  onBeforeUnmount(deactivate)
}
