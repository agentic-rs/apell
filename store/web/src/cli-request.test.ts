import { describe, expect, it } from 'vitest';
import { cliCodeFromSearch, cliLoginUrl, cliRequest, normalizeCliCode } from './cli-request';

describe('CLI browser approval', () => {
  it('normalizes typed codes without accepting a URL or ambiguous query', () => {
    expect(normalizeCliCode(' abcde-fghjk ')).toBe('ABCDE-FGHJK');
    expect(normalizeCliCode('abcdefghjk')).toBe('ABCDE-FGHJK');
    for (const value of ['ABCDE', 'ABCDE-FGHJK-extra', '//evil.test', 'ABCDE\nFGHJK'])
      expect(normalizeCliCode(value)).toBeUndefined();
    expect(cliCodeFromSearch('?code=abcde-fghjk')).toBe('ABCDE-FGHJK');
    expect(cliCodeFromSearch('?code=ABCDE-FGHJK&code=ZZZZZ-ZZZZZ')).toBeUndefined();
    expect(cliCodeFromSearch('')).toBeUndefined();
  });

  it('resumes only the fixed GitHub sign-in endpoint with a canonical code', () => {
    expect(cliLoginUrl('abcdefghjk')).toBe('/api/v1/auth/github/login?cli_code=ABCDE-FGHJK');
    expect(() => cliLoginUrl('https://elsewhere.test')).toThrow();
  });

  it('rejects a response for another terminal and malformed approval state', () => {
    const request = { user_code: 'ABCDE-FGHJK', expires_at: 2_000_000_000, status: 'pending' };
    expect(cliRequest(request, 'ABCDE-FGHJK')).toEqual(request);
    for (const patch of [
      { user_code: 'ZZZZZ-ZZZZZ' },
      { status: 'automatic_approval' },
      { status: ['pending'] },
      { expires_at: '2000000000' },
      { expires_at: -1 },
      { expires_at: Number.POSITIVE_INFINITY },
    ])
      expect(() => cliRequest({ ...request, ...patch }, 'ABCDE-FGHJK')).toThrow();
  });
});
