import { ApiClient } from '@/api/client'
import type { ApiTransport } from '@/api/contracts'
import { createHttpTransport } from '@/api/http'
import { createItemsApi, itemMockRoutes } from '@/api/items'
import { createMockTransport } from '@/api/mock'

/**
 * Build a typed API facade around any transport. The default is the in-memory
 * mock backend; setting `VITE_API_MODE=http` swaps in the HTTP transport
 * without touching stores or views.
 */
function defaultTransport(): ApiTransport {
  if (import.meta.env.VITE_API_MODE === 'http') {
    return createHttpTransport({
      baseUrl: import.meta.env.VITE_API_BASE_URL ?? '/api/v1',
    })
  }
  return createMockTransport({ routes: itemMockRoutes })
}

export function createApi(transport: ApiTransport = defaultTransport()) {
  const client = new ApiClient(transport)

  return {
    client,
    items: createItemsApi(client),
  }
}

export type Api = ReturnType<typeof createApi>

export const api = createApi()
export const apiClient = api.client

export { ApiClient } from '@/api/client'
export { ApiError, apiErrorMessage, isApiError } from '@/api/errors'
export { createHttpTransport } from '@/api/http'
export { createMockTransport } from '@/api/mock'
export { resetMockItems } from '@/api/items'
export type { ItemDto, ItemInput, ItemListQuery, ItemStatus } from '@/api/items'
export type {
  ApiErrorBody,
  ApiErrorCode,
  ApiMethod,
  ApiRequest,
  ApiResponse,
  ApiTransport,
  PageDto,
  PageQuery,
  RequestMeta,
} from '@/api/contracts'
