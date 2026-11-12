import type { ApiMethod, ApiRequest, ApiResponse, ApiTransport } from '@/api/contracts'
import { ApiError } from '@/api/errors'

export interface MockContext<TBody = unknown> {
  params: Record<string, string>
  query: URLSearchParams
  body: TBody
  request: ApiRequest
}

export type MockHandler = (context: MockContext) => unknown | Promise<unknown>

export interface MockRoute {
  method: ApiMethod
  /** Path with `:name` placeholders, e.g. `/items/:id`. */
  path: string
  handle: MockHandler
}

export interface MockTransportOptions {
  routes: MockRoute[]
  /** Fixed delay, or a `[min, max]` range. Set to 0 in tests. */
  latencyMs?: number | [number, number]
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function pathToRegExp(path: string): RegExp {
  const segments = path.split('/').map((segment) => {
    if (segment.startsWith(':')) return `(?<${segment.slice(1)}>[^/]+)`
    return escapeRegExp(segment)
  })
  return new RegExp(`^${segments.join('/')}/?$`)
}

function delayFor(options: MockTransportOptions): number {
  const latency = options.latencyMs ?? [80, 220]
  if (typeof latency === 'number') return latency
  const [min, max] = latency
  return min + Math.random() * Math.max(0, max - min)
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function newId(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
}

/**
 * A tiny in-memory HTTP-like backend for development and tests.
 *
 * Routes are matched by method + path, handlers return plain data (or throw
 * `ApiError`), and the result is wrapped in the same envelope as the HTTP
 * transport so stores and views never know the difference.
 */
export function createMockTransport(options: MockTransportOptions): ApiTransport {
  const table = options.routes.map((route) => ({
    ...route,
    matcher: pathToRegExp(route.path),
  }))

  return {
    async request<T>(request: ApiRequest): Promise<ApiResponse<T>> {
      const delay = delayFor(options)
      if (delay > 0) await wait(delay)

      const path = request.path.startsWith('/') ? request.path : `/${request.path}`
      const route = table.find(
        (candidate) =>
          candidate.method === request.method && candidate.matcher.test(path),
      )

      if (!route) {
        throw new ApiError({
          code: 'not_found',
          message: `Endpoint not found: ${request.method} ${path}`,
          status: 404,
        })
      }

      const match = route.matcher.exec(path)
      const params: Record<string, string> = match?.groups ? { ...match.groups } : {}
      const query = new URLSearchParams(
        Object.entries(request.query ?? {}).flatMap(([key, value]) =>
          value === undefined || value === null || value === ''
            ? []
            : Array.isArray(value)
              ? value.map((item) => [key, String(item)] as [string, string])
              : [[key, String(value)] as [string, string]],
        ),
      )

      let data: unknown
      try {
        data = await route.handle({
          params,
          query,
          body: request.body,
          request,
        })
      } catch (error) {
        if (error instanceof ApiError) throw error
        throw new ApiError({
          code: 'internal_error',
          message: error instanceof Error ? error.message : 'Mock handler failed',
          status: 500,
        })
      }

      return {
        data: data as T,
        status: 200,
        meta: {
          request_id: newId(),
          trace_id: newId(),
          server_time: new Date().toISOString(),
        },
        headers: {},
      }
    },
  }
}
