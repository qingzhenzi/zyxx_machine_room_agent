import { isTauri } from '@/lib/platform'

/**
 * Keep the OS autostart entry in sync with the persisted preference.
 *
 * Returns the state reported by the plugin; outside Tauri (browser preview) the
 * preference is stored locally but the OS entry cannot be changed.
 */
export async function setAutostart(enabled: boolean): Promise<boolean> {
  if (!isTauri) return false

  try {
    const autostart = await import('@tauri-apps/plugin-autostart')
    if (enabled) await autostart.enable()
    else await autostart.disable()
    return await autostart.isEnabled()
  } catch {
    return false
  }
}

export async function autostartEnabled(): Promise<boolean> {
  if (!isTauri) return false

  try {
    const autostart = await import('@tauri-apps/plugin-autostart')
    return await autostart.isEnabled()
  } catch {
    return false
  }
}

export interface DesktopBridgeHandlers {
  /** A `qestudio://` URL arrived from the OS or from a second launch. */
  openDeepLink?: (url: string) => void
  /** An item of the native (macOS) menu was activated. */
  nativeMenu?: (id: string) => void
}

/**
 * Wire OS deep links and native-menu events into the webview.
 *
 * Both the deep-link plugin callback and the `deep-link` event emitted by the
 * Rust single-instance handler are observed, because a second launch forwards
 * its argument as an event rather than through the OS URL handler.
 */
export async function initDesktopBridge(
  handlers: DesktopBridgeHandlers,
): Promise<() => void> {
  if (!isTauri) return () => {}

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
    const unlisten = await listen<string>('deep-link', (event) =>
      handlers.openDeepLink?.(event.payload),
    )
    cleanups.push(unlisten)
  } catch {
    // Ignore event bridge failures.
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

  return () => {
    for (const cleanup of cleanups) cleanup()
  }
}

/** Map a `qestudio://` deep link to an in-app route. */
export function routeFromDeepLink(url: string): string | null {
  try {
    const parsed = new URL(url)
    if (parsed.protocol !== 'qestudio:' && parsed.protocol !== 'qestudio-agent:') return null
    if (parsed.hostname === 'enroll' || parsed.pathname.startsWith('/enroll')) return '/enroll'
    if (parsed.hostname === 'logs' || parsed.pathname.startsWith('/logs')) return '/logs'
    if (parsed.hostname === 'settings' || parsed.pathname.startsWith('/settings')) return '/settings'
    return '/'
  } catch {
    return null
  }
}

/** Forward uncaught frontend errors to the Tauri log plugin (P3 log source). */
export function initFrontendLogging(): void {
  if (typeof window === 'undefined') return

  const report = (message: string, detail?: unknown) => {
    if (!isTauri) {
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
