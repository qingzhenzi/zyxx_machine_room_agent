import type { ApiErrorBody, ApiErrorCode } from '@/api/contracts'

export class ApiError extends Error {
  readonly code: ApiErrorCode
  readonly status: number
  readonly traceId?: string
  readonly details?: unknown

  constructor(options: {
    code: ApiErrorCode
    message: string
    status: number
    traceId?: string
    details?: unknown
  }) {
    super(options.message)
    this.name = 'ApiError'
    this.code = options.code
    this.status = options.status
    this.traceId = options.traceId
    this.details = options.details
  }

  static fromBody(body: ApiErrorBody, status: number, traceId?: string): ApiError {
    return new ApiError({
      code: body.error.code,
      message: body.error.message,
      status,
      traceId,
      details: body.error.details,
    })
  }

  static network(message = 'Network request failed', traceId?: string): ApiError {
    return new ApiError({ code: 'internal_error', message, status: 0, traceId })
  }
}

export function isApiError(value: unknown): value is ApiError {
  return value instanceof ApiError
}

/** Turn any thrown value into a message safe to show in the UI. */
export function apiErrorMessage(value: unknown): string {
  if (isApiError(value)) return value.message
  if (value instanceof Error) return value.message
  return 'Unknown error'
}
