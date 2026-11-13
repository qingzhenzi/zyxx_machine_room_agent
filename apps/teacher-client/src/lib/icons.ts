import type { Component } from 'vue'
import { Activity, Link2, ScrollText, Settings } from 'lucide-vue-next'

/** Icons referenced by route meta (`meta.icon`) and the activity rail. */
const appIcons: Record<string, Component> = {
  activity: Activity,
  link: Link2,
  'scroll-text': ScrollText,
  settings: Settings,
}

export function appIcon(name: string | undefined): Component {
  return (name ? appIcons[name] : undefined) ?? Activity
}
