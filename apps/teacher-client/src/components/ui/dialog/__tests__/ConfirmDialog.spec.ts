import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import { i18n } from '@/i18n'
import ConfirmDialog from '../ConfirmDialog.vue'

function mountDialog(props: Record<string, unknown> = {}) {
  return mount(ConfirmDialog, {
    props: {
      open: true,
      title: '删除这份报告？',
      description: '删除后无法恢复。',
      ...props,
    },
    global: {
      plugins: [i18n],
      stubs: { teleport: true },
    },
  })
}

describe('ConfirmDialog', () => {
  it('renders as an alert dialog with the requested tone', () => {
    const wrapper = mountDialog({ tone: 'warning' })

    expect(wrapper.text()).toContain('删除这份报告？')
    expect(wrapper.text()).toContain('删除后无法恢复。')
    expect(wrapper.find('[role="alertdialog"]').exists()).toBe(true)
    expect(wrapper.find('.dialog-tone-warning').exists()).toBe(true)
  })

  it('emits confirm and cancel from the footer actions', async () => {
    const wrapper = mountDialog()

    const buttons = wrapper.findAll('button')
    const cancel = buttons.find((button) => button.text() === '取消')
    const confirm = buttons.find((button) => button.text() === '确认')

    await cancel?.trigger('click')
    await confirm?.trigger('click')

    expect(wrapper.emitted('cancel')).toHaveLength(1)
    expect(wrapper.emitted('confirm')).toHaveLength(1)
  })

  it('disables actions while loading', () => {
    const wrapper = mountDialog({ loading: true })

    expect(wrapper.findAll('button').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
  })
})
