import type { ApiClient } from '@/api/client'
import type { PageDto } from '@/api/contracts'
import { ApiError } from '@/api/errors'
import type { MockRoute } from '@/api/mock'
import { appConfig } from '@/config/app'

export type ItemStatus = 'todo' | 'doing' | 'done'

export interface ItemDto {
  id: string
  title: string
  description: string
  status: ItemStatus
  updated_at: string
}

export interface ItemInput {
  title: string
  description?: string
  status?: ItemStatus
}

export interface ItemListQuery {
  status?: ItemStatus
  keyword?: string
}

const STORAGE_KEY = `${appConfig.storagePrefix}.items`

function daysAgo(days: number, hours = 9): string {
  return new Date(Date.now() - days * 86_400_000 - hours * 3_600_000).toISOString()
}

const seedItems: ItemDto[] = [
  {
    id: 'seed-1',
    title: 'Connect a real backend',
    description: 'Switch VITE_API_MODE to http and point VITE_API_BASE_URL at your service.',
    status: 'todo',
    updated_at: daysAgo(0),
  },
  {
    id: 'seed-2',
    title: 'Build your first screen',
    description: 'Copy src/views/ItemsView.vue and src/stores/items.ts as the reference pattern.',
    status: 'doing',
    updated_at: daysAgo(1),
  },
  {
    id: 'seed-3',
    title: 'Run the quality gate',
    description: 'pnpm check covers version consistency, lint, types, unit tests and the build.',
    status: 'done',
    updated_at: daysAgo(2),
  },
  {
    id: 'seed-4',
    title: 'Replace the demo domain',
    description: 'Remove src/api/items.ts and the demo views once your own domain is ready.',
    status: 'done',
    updated_at: daysAgo(4),
  },
  {
    id: 'seed-5',
    title: 'Configure the updater',
    description: 'Add tauri-plugin-updater after generating a signing key and a release endpoint.',
    status: 'todo',
    updated_at: daysAgo(6),
  },
]

let cache: ItemDto[] | null = null

function load(): ItemDto[] {
  if (cache) return cache
  if (typeof window === 'undefined') {
    cache = [...seedItems]
    return cache
  }

  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    cache = raw ? (JSON.parse(raw) as ItemDto[]) : [...seedItems]
  } catch {
    cache = [...seedItems]
  }
  return cache
}

function persist(items: ItemDto[]) {
  cache = items
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(items))
  } catch {
    // Persistence is best effort.
  }
}

/** Clears mock data; used by tests and the settings "reset" action. */
export function resetMockItems() {
  cache = null
  if (typeof window !== 'undefined') {
    try {
      window.localStorage.removeItem(STORAGE_KEY)
    } catch {
      // Ignore quota/security errors.
    }
  }
}

function parseBody<T>(body: unknown): T {
  if (body === undefined || body === null) {
    throw new ApiError({ code: 'bad_request', message: 'Request body is required', status: 400 })
  }
  return body as T
}

function assertTitle(title: unknown): string {
  if (typeof title !== 'string' || title.trim().length === 0) {
    throw new ApiError({ code: 'bad_request', message: 'Title is required', status: 400 })
  }
  return title.trim()
}

function isStatus(value: unknown): value is ItemStatus {
  return value === 'todo' || value === 'doing' || value === 'done'
}

/** Mock routes for the demo `items` resource. */
export const itemMockRoutes: MockRoute[] = [
  {
    method: 'GET',
    path: '/items',
    handle: ({ query }) => {
      const status = query.get('status')
      const keyword = (query.get('keyword') ?? '').trim().toLowerCase()

      const filtered = load()
        .filter((item) => (isStatus(status) ? item.status === status : true))
        .filter((item) => (keyword ? item.title.toLowerCase().includes(keyword) : true))
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at))

      const page: PageDto<ItemDto> = {
        items: filtered,
        total: filtered.length,
        page: 1,
        page_size: 50,
      }
      return page
    },
  },
  {
    method: 'POST',
    path: '/items',
    handle: ({ body }) => {
      const input = parseBody<ItemInput>(body)
      const now = new Date().toISOString()
      const item: ItemDto = {
        id: `item-${Date.now().toString(36)}`,
        title: assertTitle(input.title),
        description: (input.description ?? '').trim(),
        status: isStatus(input.status) ? input.status : 'todo',
        updated_at: now,
      }
      persist([item, ...load()])
      return item
    },
  },
  {
    method: 'PATCH',
    path: '/items/:id',
    handle: ({ params, body }) => {
      const input = parseBody<Partial<ItemInput>>(body)
      const items = load()
      const target = items.find((item) => item.id === params.id)
      if (!target) {
        throw new ApiError({ code: 'not_found', message: 'Item not found', status: 404 })
      }

      if (input.title !== undefined) target.title = assertTitle(input.title)
      if (input.description !== undefined) target.description = input.description.trim()
      if (isStatus(input.status)) target.status = input.status
      target.updated_at = new Date().toISOString()

      persist([...items])
      return target
    },
  },
  {
    method: 'DELETE',
    path: '/items/:id',
    handle: ({ params }) => {
      const items = load()
      const next = items.filter((item) => item.id !== params.id)
      if (next.length === items.length) {
        throw new ApiError({ code: 'not_found', message: 'Item not found', status: 404 })
      }
      persist(next)
      return { id: params.id }
    },
  },
]

export function createItemsApi(client: ApiClient) {
  return {
    async list(query: ItemListQuery = {}): Promise<ItemDto[]> {
      const response = await client.get<PageDto<ItemDto>>('/items', {
        query: { status: query.status, keyword: query.keyword },
      })
      return response.data.items
    },

    async create(input: ItemInput): Promise<ItemDto> {
      const response = await client.post<ItemDto>('/items', input)
      return response.data
    },

    async update(id: string, input: Partial<ItemInput>): Promise<ItemDto> {
      const response = await client.patch<ItemDto>(`/items/${id}`, input)
      return response.data
    },

    async remove(id: string): Promise<void> {
      await client.delete(`/items/${id}`)
    },
  }
}

export type ItemsApi = ReturnType<typeof createItemsApi>
