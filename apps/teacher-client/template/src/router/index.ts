import { createRouter, createWebHashHistory } from 'vue-router'
import type { RouteRecordRaw } from 'vue-router'

import { appConfig } from '@/config/app'
import { i18n } from '@/i18n'

declare module 'vue-router' {
  interface RouteMeta {
    /** i18n key, translated for tabs and the document title. */
    title?: string
    icon?: string
  }
}

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    component: () => import('@/layouts/WorkbenchLayout.vue'),
    children: [
      {
        path: '',
        name: 'home',
        component: () => import('@/views/HomeView.vue'),
        meta: { title: 'nav.home', icon: 'home' },
      },
      {
        path: 'items',
        name: 'items',
        component: () => import('@/views/ItemsView.vue'),
        meta: { title: 'nav.items', icon: 'items' },
      },
      {
        path: 'ui-kit',
        name: 'ui-kit',
        component: () => import('@/views/UiKitView.vue'),
        meta: { title: 'nav.uiKit', icon: 'ui-kit' },
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/views/SettingsView.vue'),
        meta: { title: 'nav.settings', icon: 'settings' },
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]

const router = createRouter({
  // Hash history keeps deep links working from Tauri's bundled protocol.
  history: createWebHashHistory(),
  routes,
  scrollBehavior: () => ({ top: 0 }),
})

router.afterEach((to) => {
  const title = to.meta.title ? i18n.global.t(to.meta.title) : appConfig.name
  document.title = `${title} · ${appConfig.name}`
})

export default router
