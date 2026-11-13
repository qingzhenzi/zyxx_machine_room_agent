import { isTauri } from '@tauri-apps/api/core'

import { appConfig } from '@/config/app'

export async function setAutostart(enabled: boolean): Promise<boolean> {
  if (!isTauri()) return false

  try {
    const autostart = await import('@tauri-apps/plugin-autostart')
    if (enabled) await autostart.enable()
    else await autostart.disable()
    return await autostart.isEnabled()
  } catch {
    return false
  }
}

/** Open a route in a second window. Browser preview falls back to a new tab. */
export async function openRouteInWindow(route: string, title: string): Promise<void> {
  if (!isTauri()) {
    window.open(`${window.location.origin}/#${route}`, '_blank', 'noopener')
    return
  }

  try {
    const { invoke } = await import('@tauri-apps/api/core')
    await invoke('open_workspace_window', {
      label: `workspace-${Date.now()}`,
      title,
      route,
    })
  } catch {
    // Window creation is best effort.
  }
}

export async function saveTextFile(name: string, contents: string): Promise<string | null> {
  if (!isTauri()) {
    // Browser preview: trigger a normal download instead.
    const url = URL.createObjectURL(new Blob([contents], { type: 'text/plain' }))
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = name
    anchor.click()
    URL.revokeObjectURL(url)
    return null
  }

  try {
    const { invoke } = await import('@tauri-apps/api/core')
    return await invoke<string>('save_text_file', { name, contents })
  } catch {
    return null
  }
}

export interface DesktopBridgeHandlers {
  openCommandPalette: () => void
  openDeepLink?: (url: string) => void
  nativeMenu?: (id: string) => void
}

/** Parse `scheme://route/...` links into an in-app path. */
export function routeFromDeepLink(url: string): string | null {
  try {
    const parsed = new URL(url)
    if (parsed.protocol !== `${appConfig.deeplinkScheme}:`) return null
    const [first, second] = parsed.pathname.split('/').filter(Boolean)
    if (parsed.hostname === 'settings') return '/settings'
    if (parsed.hostname === 'items') {
      return second ? `/items?item=${encodeURIComponent(second)}` : '/items'
    }
    return first ? `/${encodeURIComponent(parsed.hostname)}` : '/'
  } catch {
    return null
  }
}

export async function initDesktopBridge(
  handlers: DesktopBridgeHandlers,
): Promise<() => void> {
  if (!isTauri()) return () => {}

  const cleanups: Array<() => void> = []

  try {
    const { onOpenUrl } = await import('@tauri-apps/plugin-deep-link')
    const unlisten = await onOpenUrl((urls) => {
      for (const url of urls) handlers.openDeepLink?.(url)
    })
    cleanups.push(unlisten)
  } catch {
    // Deep links are only available on packaged desktop builds.
  }

  try {
    const { listen } = await import('@tauri-apps/api/event')
    const unlisten = await listen<string>('native-menu', (event) =>
      handlers.nativeMenu?.(event.payload),
    )
    cleanups.push(unlisten)
  } catch {
    // Ignore event bridge failures.
  }

  try {
    const shortcut = await import('@tauri-apps/plugin-global-shortcut')
    await shortcut.register('CommandOrControl+Shift+K', () =>
      handlers.openCommandPalette(),
    )
    cleanups.push(() => {
      void shortcut.unregister('CommandOrControl+Shift+K')
    })
  } catch {
    // Shortcut conflicts are non-fatal.
  }

  return () => {
    for (const cleanup of cleanups) cleanup()
  }
}

export function initFrontendLogging(): void {
  if (typeof window === 'undefined') return

  const report = (message: string, detail?: unknown) => {
    if (!isTauri()) {
      console.error(message, detail)
      return
    }

    void import('@tauri-apps/plugin-log')
      .then((log) => log.error(`${message}${detail ? `: ${String(detail)}` : ''}`))
      .catch(() => {})
  }

  window.addEventListener('error', (event) => {
    report(event.message, event.error)
  })
  window.addEventListener('unhandledrejection', (event) => {
    report('Unhandled promise rejection', event.reason)
  })
}
