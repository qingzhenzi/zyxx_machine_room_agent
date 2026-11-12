import { describe, expect, it } from 'vitest'

import { createApi } from '@/api'
import { createMockTransport } from '@/api/mock'
import { itemMockRoutes } from '@/api/items'

function testApi() {
  return createApi(createMockTransport({ routes: itemMockRoutes, latencyMs: 0 }))
}

describe('mock API', () => {
  it('lists seeded items', async () => {
    const api = testApi()
    const items = await api.items.list()
    expect(items.length).toBeGreaterThan(0)
    expect(items[0]).toHaveProperty('title')
  })

  it('creates, updates and deletes an item', async () => {
    const api = testApi()
    const created = await api.items.create({ title: 'Hello', description: 'World' })
    expect(created.status).toBe('todo')

    const updated = await api.items.update(created.id, { status: 'done' })
    expect(updated.status).toBe('done')

    await api.items.remove(created.id)
    const remaining = await api.items.list()
    expect(remaining.find((item) => item.id === created.id)).toBeUndefined()
  })

  it('rejects an empty title with a typed error', async () => {
    const api = testApi()
    await expect(api.items.create({ title: '   ' })).rejects.toMatchObject({
      code: 'bad_request',
      status: 400,
    })
  })

  it('returns not_found for unknown ids', async () => {
    const api = testApi()
    await expect(api.items.update('missing', { title: 'x' })).rejects.toMatchObject({
      code: 'not_found',
    })
  })
})
