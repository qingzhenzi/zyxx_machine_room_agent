import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import StatusBadge from '../StatusBadge.vue'

describe('StatusBadge', () => {
  it('renders the label with the matching tone class', () => {
    const wrapper = mount(StatusBadge, {
      props: { label: 'Done', tone: 'success' },
    })

    expect(wrapper.text()).toContain('Done')
    expect(wrapper.classes()).toContain('status-badge-success')
  })

  it('pulses for accent tones unless disabled', () => {
    const wrapper = mount(StatusBadge, {
      props: { label: 'Running', tone: 'accent' },
    })

    expect(wrapper.find('.status-dot').classes()).toContain('animate-pulse-dot')
  })

  it('can hide the dot', () => {
    const wrapper = mount(StatusBadge, {
      props: { label: 'Custom', tone: 'info', dot: false, pulse: false },
    })

    expect(wrapper.find('.status-dot').exists()).toBe(false)
  })
})
