import { appConfig } from '@/config/app'
import type { ApiErrorBody, ApiRequest, ApiResponse, ApiTransport } from '@/api/contracts'
import { ApiError } from '@/api/errors'

export interface HttpTransportOptions {
  baseUrl: string
  timeoutMs?: number
  fetchImpl?: typeof fetch
}

const CACHE_PREFIX = `${appConfig.storagePrefix}.http-cache.`

function readCache<T>(key: string): ApiResponse<T> | null {
  if (typeof window === 'undefined') return null
  try {
    const raw = window.localStorage.getItem(`${CACHE_PREFIX}${key}`)
    if (!raw) return null
    return JSON.parse(raw) as ApiResponse<T>
  } catch {
    return null
  }
}

function writeCache<T>(key: string, response: ApiResponse<T>): void {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(`${CACHE_PREFIX}${key}`, JSON.stringify(response))
  } catch {
    // Best effort: quota or hardened webviews can reject writes.
  }
}

function buildUrl(baseUrl: string, path: string, query?: Record<string, unknown>): string {
  const base = baseUrl.endsWith('/') ? baseUrl.slice(0, -1) : baseUrl
  const normalizedPath = path.startsWith('/') ? path : `/${path}`
  const url = new URL(`${base}${normalizedPath}`, 'http://localhost')

  for (const [key, value] of Object.entries(query ?? {})) {
    if (value === undefined || value === null || value === '') continue
    if (Array.isArray(value)) {
      for (const item of value) url.searchParams.append(key, String(item))
    } else {
      url.searchParams.set(key, String(value))
    }
  }

  // Keep relative base URLs relative for Tauri/WebView deployments.
  if (!/^https?:\/\//i.test(base)) return `${url.pathname}${url.search}`
  return url.toString()
}

async function parseBody(response: Response): Promise<unknown> {
  if (response.status === 204) return null
  const contentType = response.headers.get('content-type') ?? ''
  if (contentType.includes('application/json')) return response.json()
  return response.text()
}

function requestMeta(response: Response) {
  return {
    request_id: response.headers.get('x-request-id') ?? '',
    trace_id: response.headers.get('x-trace-id') ?? '',
    server_time: response.headers.get('x-server-time') ?? new Date().toISOString(),
  }
}

export function createHttpTransport(options: HttpTransportOptions): ApiTransport {
  const timeoutMs = options.timeoutMs ?? 15_000
  const fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis)

  return {
    async request<T>(request: ApiRequest): Promise<ApiResponse<T>> {
      const controller = new AbortController()
      const timeout = setTimeout(() => controller.abort(), timeoutMs)
      const headers: Record<string, string> = { ...request.headers }
      const url = buildUrl(options.baseUrl, request.path, request.query)
      const cacheKey = request.method === 'GET' ? url : ''

      try {
        const response = await fetchImpl(url, {
          method: request.method,
          headers,
          body: request.body === undefined ? undefined : JSON.stringify(request.body),
          signal: controller.signal,
        })

        const data = await parseBody(response)
        const meta = requestMeta(response)

        if (!response.ok) {
          const body = data as Partial<ApiErrorBody> | null
          if (body && typeof body === 'object' && 'error' in body && body.error) {
            throw ApiError.fromBody(body as ApiErrorBody, response.status, meta.trace_id)
          }
          throw new ApiError({
            status: response.status,
            code: response.status === 401 ? 'unauthorized' : 'internal_error',
            message: `HTTP ${response.status}`,
            traceId: meta.trace_id,
          })
        }

        const responseBody: ApiResponse<T> = {
          data: data as T,
          status: response.status,
          meta,
          headers: Object.fromEntries(response.headers.entries()),
        }
        if (cacheKey) writeCache(cacheKey, responseBody)
        return responseBody
      } catch (error) {
        if (error instanceof ApiError) throw error

        if (cacheKey) {
          const cached = readCache<T>(cacheKey)
          if (cached) {
            return { ...cached, meta: { ...cached.meta, from_cache: true } }
          }
        }

        if (error instanceof DOMException && error.name === 'AbortError') {
          throw ApiError.network('Request timed out')
        }
        throw ApiError.network(error instanceof Error ? error.message : undefined)
      } finally {
        clearTimeout(timeout)
      }
    },
  }
}
