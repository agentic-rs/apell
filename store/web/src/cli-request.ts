import type { CliRequest } from './types';

export function normalizeCliCode(value: string): string | undefined {
  const code = value.trim().toUpperCase();
  if (!/^[A-Z0-9]{5}-?[A-Z0-9]{5}$/.test(code)) return undefined;
  const plain = code.replace('-', '');
  return `${plain.slice(0, 5)}-${plain.slice(5)}`;
}

export function cliCodeFromSearch(search: string): string | undefined {
  const codes = new URLSearchParams(search).getAll('code');
  return codes.length === 1 ? normalizeCliCode(codes[0]) : undefined;
}

export function cliLoginUrl(code: string): string {
  const normalized = normalizeCliCode(code);
  if (!normalized) throw new Error('Enter the verification code shown in your terminal.');
  return `/api/v1/auth/github/login?cli_code=${encodeURIComponent(normalized)}`;
}

export function cliRequest(value: unknown, expected_code: string): CliRequest {
  if (
    !value ||
    typeof value !== 'object' ||
    !('user_code' in value) ||
    value.user_code !== expected_code ||
    !('expires_at' in value) ||
    typeof value.expires_at !== 'number' ||
    !Number.isSafeInteger(value.expires_at) ||
    value.expires_at <= 0 ||
    !('status' in value) ||
    typeof value.status !== 'string' ||
    !['pending', 'approved', 'denied'].includes(value.status)
  ) {
    throw new Error(
      'The Store returned an invalid CLI request. Start login again in your terminal.',
    );
  }
  return value as CliRequest;
}
