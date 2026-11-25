import { describe, expect, it } from 'vitest';
import { accountReturnTo } from './account-navigation';
import { signInOption } from './sign-in';
import type { Session } from './types';

const store = 'https://store.example.invalid';
const session: Session = {
  login_available: true,
  login_provider: 'account',
  user: null,
  csrf_token: null,
};

describe('returning to an account page after sign-in', () => {
  it('keeps only recognized account pages and a single recognized package tab', () => {
    expect(accountReturnTo(new URL('/account/access/?tab=people#details', store))).toBe(
      '/account/access',
    );
    expect(accountReturnTo(new URL('/account/new?auth_error=failed', store))).toBe('/account/new');
    for (const tab of ['releases', 'publishing', 'people']) {
      const path = `/account/packages/field-notes_2?tab=${tab}`;
      expect(accountReturnTo(new URL(`${path}&auth_error=github_link_required`, store))).toBe(path);
    }
    for (const search of ['?tab=other', '?tab=people&tab=publishing', '?tab=people&tab=people']) {
      expect(accountReturnTo(new URL(`/account/packages/field-notes${search}`, store))).toBe(
        '/account/packages/field-notes',
      );
    }
  });

  it('keeps account home and CLI sign-in on their existing paths', () => {
    for (const path of [
      '/account',
      '/account/cli?code=ABCDE-FGHJK',
      '/applets/field-notes',
      '/account/packages/Invalid',
      '/account/packages/' + 'a'.repeat(81),
      '/account/packages/people%2Fother',
    ]) {
      expect(accountReturnTo(new URL(path, store))).toBeUndefined();
      expect(signInOption(session, new URL(path, store))?.url).toBe('/api/v1/auth/account/login');
    }
  });

  it('encodes the destination in both providers’ links and drops callback errors', () => {
    const page = new URL('/account/packages/field-notes/?tab=people&auth_error=failed', store);
    for (const login_provider of ['account', 'github'] as const) {
      const option = signInOption({ ...session, login_provider }, page);
      const redirect = new URL(option!.url, store);
      expect(redirect.pathname).toBe(`/api/v1/auth/${login_provider}/login`);
      expect([...redirect.searchParams]).toEqual([
        ['return_to', '/account/packages/field-notes?tab=people'],
      ]);
    }
  });
});
