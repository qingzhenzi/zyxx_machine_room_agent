import { appConfig } from '@/config/app'
import { onMounted, ref } from 'vue'
import { invoke, isTauri } from '@tauri-apps/api/core'

export interface PlatformInfo {
  appVersion: string
  tauriVersion: string
  os: string
  arch: string
}

const fallback: PlatformInfo = {
  appVersion: appConfig.version,
  tauriVersion: '—',
  os: 'browser',
  arch: 'web',
}

export function usePlatformInfo() {
  const info = ref<PlatformInfo>({ ...fallback })
  const loading = ref(false)

  onMounted(async () => {
    if (!isTauri()) return

    loading.value = true
    try {
      info.value = await invoke<PlatformInfo>('platform_info')
    } catch {
      info.value = { ...fallback, os: 'unknown' }
    } finally {
      loading.value = false
    }
  })

  return { info, loading }
}
