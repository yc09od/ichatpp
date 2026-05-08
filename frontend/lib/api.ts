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
//   - **TODO [38]**: a 401 response triggers one transparent
//     `/api/auth/refresh` attempt; on success the original request is
//     retried once with the new cookies; on failure the original 401 is
//     surfaced. Concurrent 401s share a single refresh promise so a tab
//     reload doesn't fan out into N refresh requests.

const API_BASE = process.env.NEXT_PUBLIC_API_BASE_URL ?? '';

const MUTATION_METHODS = new Set(['POST', 'PUT', 'PATCH', 'DELETE']);

/// Endpoints that must NEVER trigger refresh-on-401: the refresh endpoint
/// itself (would loop), and login/logout (a 401 there is a real auth
/// failure, not a stale access token).
const NO_REFRESH_PATHS = new Set([
  '/api/auth/refresh',
  '/api/auth/login',
  '/api/auth/logout',
  '/api/auth/register',
]);

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
  /** Internal — set when this call is itself a retry, so we don't retry-on-retry. */
  _isRetry?: boolean;
}

// ────────────────────────────────────────────────────────────────────────
// Refresh-on-401 plumbing
// ────────────────────────────────────────────────────────────────────────

// Outstanding refresh, if any. Multiple concurrent 401s await the same
// promise — without this, ten parallel queries hitting a stale access
// token would fan out into ten /refresh requests.
let inflightRefresh: Promise<boolean> | null = null;

// Optional listener for a definitive auth failure (refresh failed). The
// (app) layout subscribes here and routes the user back to /login.
type AuthFailureListener = () => void;
const authFailureListeners = new Set<AuthFailureListener>();

export function onAuthFailure(listener: AuthFailureListener): () => void {
  authFailureListeners.add(listener);
  return () => authFailureListeners.delete(listener);
}

function notifyAuthFailure(): void {
  // Snapshot via Array.from — iterating a Set directly trips
  // --downlevelIteration since the project's TS target predates ES2015
  // for-of-on-iterables.
  Array.from(authFailureListeners).forEach((l) => {
    try {
      l();
    } catch {
      // listener errors must not surface here.
    }
  });
}

/**
 * Hit `/api/auth/refresh`. Returns true on success, false otherwise.
 * Coalesces concurrent callers onto a single in-flight request.
 */
async function refreshAccessToken(): Promise<boolean> {
  if (inflightRefresh) return inflightRefresh;
  inflightRefresh = (async () => {
    try {
      const res = await fetch(`${API_BASE}/api/auth/refresh`, {
        method: 'POST',
        credentials: 'include',
        headers: (() => {
          const h = new Headers();
          const csrf = readCookie('csrf_token');
          if (csrf) h.set('X-CSRF-Token', csrf);
          return h;
        })(),
      });
      return res.ok;
    } catch {
      return false;
    } finally {
      // Clear after the awaiter chain settles so a second wave of 401s
      // creates a fresh refresh attempt.
      inflightRefresh = null;
    }
  })();
  return inflightRefresh;
}

// ────────────────────────────────────────────────────────────────────────
// Public entry
// ────────────────────────────────────────────────────────────────────────

export async function apiFetch<T = unknown>(
  path: string,
  options: ApiFetchOptions = {},
): Promise<T> {
  const { json, rawBody, headers: rawHeaders, method, _isRetry, ...rest } = options;
  const resolvedMethod = (
    method ?? (json !== undefined || rawBody !== undefined ? 'POST' : 'GET')
  ).toUpperCase();
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

  if (res.status === 401 && !_isRetry && !NO_REFRESH_PATHS.has(path)) {
    // Try to refresh once; if it succeeds, replay the original request
    // exactly once. The CSRF token may have rotated, so we re-read it
    // inside the recursive call.
    const refreshed = await refreshAccessToken();
    if (refreshed) {
      return apiFetch<T>(path, { ...options, _isRetry: true });
    }
    notifyAuthFailure();
  }

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
