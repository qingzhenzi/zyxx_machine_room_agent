import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it } from 'vitest'

import { useTabsStore } from '../tabs'

const STORAGE_KEY = 'qestudio.tabs'

describe('tabs store', () => {
  beforeEach(() => {
    window.localStorage.clear()
    setActivePinia(createPinia())
  })

  it('starts with the pinned status tab', () => {
    const tabs = useTabsStore()

    expect(tabs.tabs).toHaveLength(1)
    expect(tabs.tabs[0]).toMatchObject({ path: '/', title: '状态', pinned: true })
    expect(tabs.activePath).toBe('/')
  })

  it('opens a tab, keeps it unique and updates its title', () => {
    const tabs = useTabsStore()

    tabs.open({ path: '/enroll', title: '注册', icon: 'link' })
    tabs.open({ path: '/enroll', title: 'Enrollment', icon: 'link' })

    expect(tabs.tabs).toHaveLength(2)
    expect(tabs.tabs[1]?.title).toBe('Enrollment')
    expect(tabs.activePath).toBe('/enroll')
  })

  it('closes the active tab and selects its neighbour', () => {
    const tabs = useTabsStore()

    tabs.open({ path: '/enroll', title: '注册', icon: 'link' })
    tabs.open({ path: '/logs', title: '日志', icon: 'scroll-text' })

    expect(tabs.close('/logs')).toBe('/enroll')
    expect(tabs.activePath).toBe('/enroll')
    expect(tabs.tabs.map((tab) => tab.path)).toEqual(['/', '/enroll'])
  })

  it('never closes the pinned tab', () => {
    const tabs = useTabsStore()

    expect(tabs.close('/')).toBeNull()
    expect(tabs.tabs).toHaveLength(1)
  })

  it('closes other tabs while keeping the requested one', () => {
    const tabs = useTabsStore()

    tabs.open({ path: '/enroll', title: '注册', icon: 'link' })
    tabs.open({ path: '/logs', title: '日志', icon: 'scroll-text' })
    tabs.closeOthers('/enroll')

    expect(tabs.tabs.map((tab) => tab.path)).toEqual(['/', '/enroll'])
    expect(tabs.activePath).toBe('/enroll')
  })

  it('restores tabs and the active path from localStorage', () => {
    const tabs = useTabsStore()
    tabs.open({ path: '/enroll', title: '注册', icon: 'link' })
    tabs.open({ path: '/logs', title: '日志', icon: 'scroll-text' })

    setActivePinia(createPinia())
    const restored = useTabsStore()

    expect(restored.tabs.map((tab) => tab.path)).toEqual(['/', '/enroll', '/logs'])
    expect(restored.activePath).toBe('/logs')
  })

  it('drops persisted tabs that are not routes of the agent shell', () => {
    window.localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        tabs: [
          { path: '/', title: '状态', icon: 'activity', pinned: true },
          { path: '/projects', title: '测试项目', icon: 'folder-kanban' },
          { path: '/logs', title: '日志', icon: 'scroll-text' },
        ],
        activePath: '/projects',
      }),
    )

    setActivePinia(createPinia())
    const tabs = useTabsStore()

    expect(tabs.tabs.map((tab) => tab.path)).toEqual(['/', '/logs'])
    expect(tabs.activePath).toBe('/')
  })
})
