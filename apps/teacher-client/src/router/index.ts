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
    name: 'home',
    component: () => import('@/views/TeacherHome.vue'),
    meta: { title: '教师工作台' },
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
    ? `${i18n.global.t(to.meta.title)} · QEStudio Teacher`
    : 'QEStudio Teacher'
})

export default router
