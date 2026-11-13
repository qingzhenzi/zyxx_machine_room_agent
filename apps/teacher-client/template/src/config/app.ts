/**
 * Single source of truth for app-level metadata used by the UI.
 * `scripts/init.mjs` rewrites this file together with package.json,
 * tauri.conf.json and Cargo.toml when the template is instantiated.
 */
export const appConfig = {
  /** Product name shown in the title bar, About panel and window title. */
  name: 'Desktop Starter',
  /** Short name used where space is tight. */
  shortName: 'Starter',
  version: '0.1.0',
  /** Prefix for localStorage keys. Lowercase, no spaces. */
  storagePrefix: 'desktop-starter',
  /** Custom URL scheme registered for deep links (no `://`). */
  deeplinkScheme: 'desktopstarter',
  repository: '',
} as const
