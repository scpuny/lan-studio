import { reactive } from 'vue'
import type { ApiErrorCode, ApiErrorDetail } from './types'

/** Console-side session: memory + sessionStorage, never written to disk. */
export interface SessionState {
  token: string
  password: string
  tokenRequired: boolean
  /** `null` until the first `/api/status` or `/api/auth` answer arrives. */
  passwordSet: boolean | null
  ready: boolean
}

export const session = reactive<SessionState>({
  token: '',
  password: '',
  tokenRequired: false,
  passwordSet: null,
  ready: false,
})

const TOKEN_KEY = 'lanstudio.token'
const PASSWORD_KEY = 'lanstudio.password'

try {
  session.token = sessionStorage.getItem(TOKEN_KEY) ?? ''
  session.password = sessionStorage.getItem(PASSWORD_KEY) ?? ''
} catch {
  /* private mode: memory-only session */
}

export class ApiError extends Error {
  readonly status: number
  readonly code: ApiErrorCode
  readonly field: string

  constructor(status: number, code: ApiErrorCode, message: string, field = '') {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.field = field
  }
}

export function isApiError(value: unknown): value is ApiError {
  return value instanceof ApiError
}

/** `setup_required` / `auth_failed` both mean "unlock the console first". */
export function isAuthError(value: unknown): value is ApiError {
  return isApiError(value) && (value.code === 'setup_required' || value.code === 'auth_failed')
}

export function errorMessage(value: unknown): string {
  if (value instanceof Error) return value.message
  return String(value)
}

type Method = 'GET' | 'POST'

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

function authHeaders(extra: Record<string, string> = {}): Record<string, string> {
  const headers: Record<string, string> = { ...extra }
  if (session.token) headers['X-Admin-Token'] = session.token
  if (session.password) headers['X-Admin-Password'] = session.password
  return headers
}

function parseJson(text: string): unknown {
  if (!text) return null
  try {
    return JSON.parse(text) as unknown
  } catch {
    return null
  }
}

function toDetail(value: unknown): ApiErrorDetail {
  if (!isRecord(value)) return { code: 'internal_error', message: '' }
  const code = typeof value.code === 'string' ? (value.code as ApiErrorCode) : 'internal_error'
  const message = typeof value.message === 'string' ? value.message : ''
  const field = typeof value.field === 'string' ? value.field : undefined
  return field === undefined ? { code, message } : { code, message, field }
}

function readError(payload: unknown, status: number): ApiError {
  const detail = toDetail(isRecord(payload) ? payload.error : null)
  return new ApiError(
    status,
    detail.code,
    detail.message || `HTTP ${status}`,
    detail.field ?? '',
  )
}

/**
 * Call one JSON endpoint and unwrap the M0 envelope.
 *
 * `T` is the `data` payload, not the envelope: callers never see `ok` /
 * `request_id` because a non-`ok` answer is always thrown as `ApiError`.
 */
export async function request<T>(method: Method, path: string, body?: unknown): Promise<T> {
  const headers = authHeaders()
  const init: RequestInit = { method, headers, cache: 'no-store' }
  if (body !== undefined) {
    headers['Content-Type'] = 'application/json'
    init.body = JSON.stringify(body)
  }

  let response: Response
  try {
    response = await fetch(path, init)
  } catch (cause) {
    throw new ApiError(0, 'internal_error', `无法连接管理接口：${errorMessage(cause)}`)
  }

  const payload = parseJson(await response.text())
  if (!response.ok || (isRecord(payload) && payload.ok === false)) {
    throw readError(payload, response.status)
  }
  if (isRecord(payload) && 'data' in payload) return payload.data as T
  return payload as T
}

export const get = <T>(path: string): Promise<T> => request<T>('GET', path)

export const post = <T>(path: string, body: unknown = {}): Promise<T> =>
  request<T>('POST', path, body)

/** Non-JSON endpoint (Prometheus exposition, CSV export). */
export async function getText(path: string): Promise<string> {
  let response: Response
  try {
    response = await fetch(path, { headers: authHeaders(), cache: 'no-store' })
  } catch (cause) {
    throw new ApiError(0, 'internal_error', `无法连接管理接口：${errorMessage(cause)}`)
  }
  const text = await response.text()
  if (!response.ok) throw new ApiError(response.status, 'internal_error', `HTTP ${response.status}`)
  return text
}

export function storeCredentials(token: string, password: string): void {
  session.token = token || ''
  session.password = password || ''
  try {
    sessionStorage.setItem(TOKEN_KEY, session.token)
    sessionStorage.setItem(PASSWORD_KEY, session.password)
  } catch {
    /* ignore */
  }
}

export function clearPassword(): void {
  session.password = ''
  try {
    sessionStorage.removeItem(PASSWORD_KEY)
  } catch {
    /* ignore */
  }
}
