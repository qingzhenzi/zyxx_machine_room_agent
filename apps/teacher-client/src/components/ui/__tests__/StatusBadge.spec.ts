import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import StatusBadge from '../StatusBadge.vue'

function mountBadge(props: Record<string, unknown> = {}) {
  return mount(StatusBadge, {
    props: { label: '已注册', tone: 'success', ...props },
  })
}

describe('StatusBadge', () => {
  it('renders a label with the requested tone', () => {
    const wrapper = mountBadge()

    expect(wrapper.text()).toContain('已注册')
    expect(wrapper.classes()).toContain('status-badge-success')
    expect(wrapper.find('.status-dot').exists()).toBe(true)
  })

  it('pulses for the accent tone and supports explicit pulse', () => {
    const accent = mountBadge({ label: '注册中', tone: 'accent' })
    expect(accent.classes()).toContain('status-badge-accent')
    expect(accent.find('.status-dot').classes()).toContain('animate-pulse-dot')

    const muted = mountBadge({ label: '未注册', tone: 'muted', pulse: true })
    expect(muted.find('.status-dot').classes()).toContain('animate-pulse-dot')
  })

  it('can hide the dot', () => {
    const wrapper = mountBadge({ dot: false })
    expect(wrapper.find('.status-dot').exists()).toBe(false)
  })
})
