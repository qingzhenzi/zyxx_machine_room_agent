import { isTauri } from '@tauri-apps/api/core'
import { openUrl } from '@tauri-apps/plugin-opener'

/** Open a link in the OS browser (desktop) or a new tab (browser preview). */
export async function openExternal(url: string) {
  if (isTauri()) {
    try {
      await openUrl(url)
      return
    } catch {
      // Fall through to the browser behavior below.
    }
  }

  window.open(url, '_blank', 'noopener,noreferrer')
}
