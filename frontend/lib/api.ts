// HTTP client for ichatpp backend.
//
// Conventions enforced here (see context.md "技术约束 — 前端认证"):
//   - All requests use `credentials: 'include'` so the browser sends the
//     httpOnly access_token / refresh_token cookies.
//   - Mutation requests (POST / PUT / PATCH / DELETE) attach the
//     `X-CSRF-Token` header read from the non-httpOnly `csrf_token` cookie
//     (double-submit cookie pattern).
//   - Success bodies follow ARCHITECTURE.md §6: `{ data, meta }` — we unwrap
//     and return `data`.
//   - Failure bodies follow `{ error: { code, message, details } }` — we
//     throw `ApiError` carrying those fields plus HTTP status.

const API_BASE = process.env.NEXT_PUBLIC_API_BASE_URL ?? '';

const MUTATION_METHODS = new Set(['POST', 'PUT', 'PATCH', 'DELETE']);

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly details?: unknown,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

interface ApiSuccess<T> {
  data: T;
  meta?: { timestamp?: string };
}

interface ApiFailure {
  error: { code: string; message: string; details?: unknown };
}

function readCookie(name: string): string | undefined {
  if (typeof document === 'undefined') return undefined;
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = document.cookie.match(new RegExp(`(?:^|;\\s*)${escaped}=([^;]+)`));
  return match ? decodeURIComponent(match[1]) : undefined;
}

export interface ApiFetchOptions extends Omit<RequestInit, 'body'> {
  /** JSON-serializable body. If you need raw bodies (FormData, Blob), pass `rawBody`. */
  json?: unknown;
  /** Bypass JSON serialization. */
  rawBody?: BodyInit;
}

export async function apiFetch<T = unknown>(
  path: string,
  options: ApiFetchOptions = {},
): Promise<T> {
  const { json, rawBody, headers: rawHeaders, method, ...rest } = options;
  const resolvedMethod = (method ?? (json !== undefined || rawBody !== undefined ? 'POST' : 'GET')).toUpperCase();
  const headers = new Headers(rawHeaders);

  let body: BodyInit | undefined;
  if (rawBody !== undefined) {
    body = rawBody;
  } else if (json !== undefined) {
    if (!headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
    body = JSON.stringify(json);
  }

  if (MUTATION_METHODS.has(resolvedMethod)) {
    const csrf = readCookie('csrf_token');
    if (csrf) headers.set('X-CSRF-Token', csrf);
  }

  const url = path.startsWith('http') ? path : `${API_BASE}${path}`;
  const res = await fetch(url, {
    ...rest,
    method: resolvedMethod,
    headers,
    body,
    credentials: 'include',
  });

  if (!res.ok) {
    let parsed: ApiFailure | undefined;
    try {
      parsed = (await res.json()) as ApiFailure;
    } catch {
      // Non-JSON failure body — fall through with HTTP status only.
    }
    const err = parsed?.error;
    throw new ApiError(
      res.status,
      err?.code ?? `HTTP_${res.status}`,
      err?.message ?? res.statusText,
      err?.details,
    );
  }

  if (res.status === 204) return undefined as T;
  const payload = (await res.json()) as ApiSuccess<T>;
  return payload.data;
}
