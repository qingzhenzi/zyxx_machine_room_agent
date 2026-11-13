import { onMounted, onUnmounted } from 'vue'
import type { Router } from 'vue-router'

import { useUiStore } from '@/stores/ui'

/**
 * Global (window-level) shortcuts. Registration happens once per layout and
 * is skipped while the user is typing into a field.
 */
export function useAppShortcuts(router: Router) {
  const ui = useUiStore()

  function isTyping(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false
    return (
      target.isContentEditable ||
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      target instanceof HTMLSelectElement
    )
  }

  function onKeydown(event: KeyboardEvent) {
    const mod = event.metaKey || event.ctrlKey
    if (!mod) return

    if (event.key.toLowerCase() === 'k') {
      event.preventDefault()
      ui.toggleCommand()
      return
    }

    if (isTyping(event.target)) return

    const targets: Record<string, string> = {
      '1': '/',
      '2': '/items',
      '3': '/ui-kit',
      '4': '/settings',
    }
    const target = targets[event.key]
    if (target) {
      event.preventDefault()
      void router.push(target)
    }
  }

  onMounted(() => window.addEventListener('keydown', onKeydown))
  onUnmounted(() => window.removeEventListener('keydown', onKeydown))
}
