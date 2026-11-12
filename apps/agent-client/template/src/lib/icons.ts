import type { Component } from 'vue'
import { Layers, LayoutDashboard, ListChecks, Settings } from 'lucide-vue-next'

export const appIcons: Record<string, Component> = {
  home: LayoutDashboard,
  items: ListChecks,
  settings: Settings,
  'ui-kit': Layers,
}

export function iconByName(name: string): Component {
  return appIcons[name] ?? LayoutDashboard
}
