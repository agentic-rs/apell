import { accountReturnTo } from './account-navigation';
import { element, link } from './dom';
import type { Session } from './types';

export const loginOptions = {
  account: {
    provider: 'account',
    label: 'Continue with Apell Account',
    url: '/api/v1/auth/account/login',
  },
  github: {
    provider: 'github',
    label: 'Continue with GitHub',
    url: '/api/v1/auth/github/login',
  },
} as const;

export function signInOption(session: Session, location?: Pick<Location, 'pathname' | 'search'>) {
  const provider = session.login_provider;
  if (session.login_available && (provider === 'account' || provider === 'github')) {
    const option = loginOptions[provider];
    const return_to = location && accountReturnTo(location);
    if (return_to) return { ...option, url: `${option.url}?${new URLSearchParams({ return_to })}` };
    return option;
  }
}

export function accountSecurityLink(): HTMLAnchorElement {
  const settings = link('Open Account Security', '/api/v1/auth/account/security', 'text-link');
  const arrow = element('span', '', '↗');
  arrow.setAttribute('aria-hidden', 'true');
  settings.append(arrow);
  settings.target = '_blank';
  settings.rel = 'noopener noreferrer';
  return settings;
}

export function signInNotice(session: Session): HTMLElement | undefined {
  const error = new URLSearchParams(location.search).get('auth_error');
  if (!error) return;

  const notice = element('p', 'notice error');
  notice.setAttribute('role', 'alert');
  const account_sign_in = signInOption(session)?.provider === 'account';
  if (error === 'github_link_required' && account_sign_in) {
    notice.textContent =
      'Link the GitHub account you use for publishing, then return here and sign in again. ';
    notice.append(accountSecurityLink());
  } else if (error === 'account_unavailable' && account_sign_in) {
    notice.textContent =
      'Apell Account is temporarily unavailable. Try signing in again in a moment.';
  } else {
    notice.textContent = 'Sign-in could not be completed. Please try again.';
  }
  return notice;
}
