export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
  ) {
    super(message);
  }
}

function errorDetails(body: unknown): { code?: string; message?: string } {
  if (!body || typeof body !== 'object' || !('error' in body)) return {};
  const detail = body.error;
  if (!detail || typeof detail !== 'object') return {};
  return {
    code: 'code' in detail && typeof detail.code === 'string' ? detail.code : undefined,
    message:
      'message' in detail && typeof detail.message === 'string' && detail.message.length <= 1000
        ? detail.message
        : undefined,
  };
}

export function errorMessage(status: number, body: unknown): string {
  const detail = errorDetails(body);
  if (status === 401) return 'Your session has expired. Sign in again.';
  if (status === 403 && detail.code === 'invalid_session_request')
    return 'Your sign-in state changed. Refresh this page and try again.';
  if (status === 403)
    return 'You no longer have permission for this action. Refresh this page to check your access.';
  if (status === 429 || status === 503) return 'The Store is busy. Please try again shortly.';
  if (status >= 500) return 'The Store is unavailable right now. Please try again.';
  if (detail.message) return detail.message;
  return status === 404
    ? 'This item could not be found.'
    : 'This request could not be completed. Please try again.';
}

export async function api<T>(
  path: string,
  options: {
    method?: 'GET' | 'POST' | 'PUT' | 'DELETE';
    body?: unknown;
    csrf_token?: string | null;
    signal?: AbortSignal;
  } = {},
): Promise<T> {
  if (!path.startsWith('/api/') || path.includes('\\') || path.startsWith('//'))
    throw new Error('Invalid Store endpoint.');
  const method = options.method ?? 'GET';
  const headers: Record<string, string> = { Accept: 'application/json' };
  let body: BodyInit | undefined;
  if (method !== 'GET') {
    if (!options.csrf_token) throw new ApiError('Sign in before making changes.', 401);
    headers['x-csrf-token'] = options.csrf_token;
    if (options.body instanceof FormData) body = options.body;
    else {
      headers['Content-Type'] = 'application/json';
      body = JSON.stringify(options.body ?? {});
    }
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body,
      signal: options.signal,
      credentials: 'same-origin',
      redirect: 'error',
    });
  } catch (error) {
    if (options.signal?.aborted) throw error;
    throw new Error('Check your connection and try again.');
  }
  const result: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const { code } = errorDetails(result);
    throw new ApiError(errorMessage(response.status, result), response.status, code);
  }
  return result as T;
}
