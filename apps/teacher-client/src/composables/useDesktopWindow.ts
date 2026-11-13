import { onMounted, onUnmounted, ref } from 'vue'
import { isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'

export function useDesktopWindow() {
  const desktop = isTauri()
  const windowHandle = desktop ? getCurrentWindow() : null
  const maximized = ref(false)
  const focused = ref(true)
  let unlistenResize: (() => void) | undefined
  let unlistenFocus: (() => void) | undefined

  async function refreshMaximized() {
    if (!windowHandle) return
    try {
      maximized.value = await windowHandle.isMaximized()
    } catch {
      // Permissions or platform limits; keep the last known state.
    }
  }

  async function minimize() {
    if (!windowHandle) return
    try {
      await windowHandle.minimize()
    } catch {
      // No-op in a browser preview.
    }
  }

  async function toggleMaximize() {
    if (!windowHandle) return
    try {
      await windowHandle.toggleMaximize()
      await refreshMaximized()
    } catch {
      // No-op in a browser preview.
    }
  }

  async function close() {
    if (!windowHandle) return
    try {
      await windowHandle.close()
    } catch {
      // No-op in a browser preview.
    }
  }

  onMounted(async () => {
    if (!windowHandle) return

    try {
      await refreshMaximized()
      unlistenResize = await windowHandle.onResized(() => {
        void refreshMaximized()
      })
      unlistenFocus = await windowHandle.onFocusChanged(({ payload }) => {
        focused.value = payload
      })
    } catch {
      // Older Tauri builds may not expose one of these events.
    }
  })

  onUnmounted(() => {
    unlistenResize?.()
    unlistenFocus?.()
  })

  return {
    desktop,
    maximized,
    focused,
    minimize,
    toggleMaximize,
    close,
  }
}
