export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad|iPod/.test(navigator.userAgent)

export const modKey = isMac ? '⌘' : 'Ctrl'
export const altKey = isMac ? '⌥' : 'Alt'

export const isTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
