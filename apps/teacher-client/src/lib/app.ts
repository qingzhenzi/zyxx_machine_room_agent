import { isTauri } from '@/lib/platform'

/**
 * Version shown by the placeholder status page.
 *
 * TODO(P3): read the live `qe-agent` version through IPC instead of the bundled
 * frontend version. Until then this mirrors package.json / Cargo.toml.
 */
export const FALLBACK_APP_VERSION = '0.1.0'

export async function appVersion(): Promise<string> {
  if (!isTauri) return FALLBACK_APP_VERSION

  try {
    const { getVersion } = await import('@tauri-apps/api/app')
    return await getVersion()
  } catch {
    return FALLBACK_APP_VERSION
  }
}
