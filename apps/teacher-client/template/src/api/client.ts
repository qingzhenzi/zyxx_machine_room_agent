import type { ApiMethod, ApiResponse, ApiTransport } from '@/api/contracts'
import { ApiError } from '@/api/errors'

export interface RequestOptions {
  query?: Record<string, unknown>
  headers?: Record<string, string>
  /** Attach the current bearer token. Defaults to true. */
  auth?: boolean
}

type UnauthorizedHandler = () => Promise<boolean>

export class ApiClient {
  private accessToken: string | null = null
  private unauthorizedHandler?: UnauthorizedHandler

  constructor(private readonly transport: ApiTransport) {}

  setAccessToken(token: string | null) {
    this.accessToken = token
  }

  /** Retry once after an `unauthorized` response when the handler returns true. */
  setUnauthorizedHandler(handler: UnauthorizedHandler | undefined) {
    this.unauthorizedHandler = handler
  }

  getAccessToken(): string | null {
    return this.accessToken
  }

  async request<T>(
    method: ApiMethod,
    path: string,
    body?: unknown,
    options: RequestOptions = {},
    retried = false,
  ): Promise<ApiResponse<T>> {
    const headers: Record<string, string> = { ...options.headers }

    if (options.auth !== false && this.accessToken) {
      headers.Authorization = `Bearer ${this.accessToken}`
    }

    if (body !== undefined) {
      headers['Content-Type'] = 'application/json'
    }

    try {
      return await this.transport.request<T>({
        method,
        path,
        query: options.query,
        body,
        headers,
      })
    } catch (error) {
      if (
        error instanceof ApiError &&
        error.code === 'unauthorized' &&
        !retried &&
        options.auth !== false &&
        this.accessToken &&
        this.unauthorizedHandler
      ) {
        const refreshed = await this.unauthorizedHandler()
        if (refreshed) {
          return this.request<T>(method, path, body, options, true)
        }
      }

      if (error instanceof ApiError) throw error
      throw ApiError.network(error instanceof Error ? error.message : undefined)
    }
  }

  get<T>(path: string, options?: RequestOptions) {
    return this.request<T>('GET', path, undefined, options)
  }

  post<T>(path: string, body?: unknown, options?: RequestOptions) {
    return this.request<T>('POST', path, body, options)
  }

  patch<T>(path: string, body?: unknown, options?: RequestOptions) {
    return this.request<T>('PATCH', path, body, options)
  }

  put<T>(path: string, body?: unknown, options?: RequestOptions) {
    return this.request<T>('PUT', path, body, options)
  }

  delete<T>(path: string, options?: RequestOptions) {
    return this.request<T>('DELETE', path, undefined, options)
  }
}
