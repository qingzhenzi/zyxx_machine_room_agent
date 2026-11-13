import { afterEach, describe, expect, it } from 'vitest'

import { i18n, setLocale } from '@/i18n'
import { formatNumber, relativeFromNow } from '@/lib/format'

afterEach(() => {
  setLocale('zh-CN')
})

describe('i18n', () => {
  it('translates UI phrases and interpolations', () => {
    setLocale('en-US')
    expect(i18n.global.t('状态')).toBe('Status')
    expect(i18n.global.t('未注册')).toBe('Not registered')
    expect(i18n.global.t('{count} 个结果', { count: 3 })).toBe('3 results')

    setLocale('zh-CN')
    expect(i18n.global.t('状态')).toBe('状态')
    expect(i18n.global.t('{count} 个结果', { count: 3 })).toBe('3 个结果')
  })

  it('localizes numbers and relative time', () => {
    setLocale('en-US')
    expect(formatNumber(1284)).toBe('1,284')
    expect(relativeFromNow(new Date(Date.now() - 120_000).toISOString())).toBe('2 min ago')

    setLocale('zh-CN')
    expect(formatNumber(1284)).toBe('1,284')
    expect(relativeFromNow(new Date(Date.now() - 120_000).toISOString())).toBe('2 分钟前')
  })
})
