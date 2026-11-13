import { describe, expect, it } from 'vitest'

import enUS from '../locales/en-US'
import zhCN from '../locales/zh-CN'

type Dictionary = Record<string, unknown>

function flattenKeys(value: Dictionary, prefix = ''): string[] {
  return Object.entries(value).flatMap(([key, nested]) => {
    const path = prefix ? `${prefix}.${key}` : key
    return nested && typeof nested === 'object' && !Array.isArray(nested)
      ? flattenKeys(nested as Dictionary, path)
      : [path]
  })
}

describe('i18n dictionaries', () => {
  it('declare the same keys', () => {
    const zh = flattenKeys(zhCN).sort()
    const en = flattenKeys(enUS).sort()
    expect(en).toEqual(zh)
  })

  it('have no duplicate leaf paths', () => {
    for (const dict of [zhCN, enUS]) {
      const keys = flattenKeys(dict)
      expect(new Set(keys).size).toBe(keys.length)
    }
  })
})
