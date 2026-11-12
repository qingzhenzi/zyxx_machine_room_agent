import { createRouter, createWebHashHistory } from 'vue-router'
import type { RouteRecordRaw } from 'vue-router'

import { i18n } from '@/i18n'

declare module 'vue-router' {
  interface RouteMeta {
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
        name: 'status',
        component: () => import('@/views/StatusView.vue'),
        meta: { title: '状态', icon: 'activity' },
      },
      {
        path: 'setup',
        name: 'setup',
        component: () => import('@/views/SetupView.vue'),
        meta: { title: '初始化', icon: 'wrench' },
      },
      {
        path: 'enroll',
        name: 'enroll',
        component: () => import('@/views/EnrollView.vue'),
        meta: { title: '注册', icon: 'link' },
      },
      {
        path: 'logs',
        name: 'logs',
        component: () => import('@/views/LogsView.vue'),
        meta: { title: '日志', icon: 'scroll-text' },
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/views/SettingsView.vue'),
        meta: { title: '设置', icon: 'settings' },
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
  document.title = to.meta.title
    ? `${i18n.global.t(to.meta.title)} · QEStudio Agent`
    : 'QEStudio Agent'
})

export default router
