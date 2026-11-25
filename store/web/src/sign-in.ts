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

export function signInOption(session: Session) {
  const provider = session.login_provider;
  if (session.login_available && (provider === 'account' || provider === 'github')) {
    return loginOptions[provider];
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
  if (error === 'github_link_required' && signInOption(session)?.provider === 'account') {
    notice.textContent =
      'Link the GitHub account you use for publishing, then return here and sign in again. ';
    notice.append(accountSecurityLink());
  } else {
    notice.textContent = 'Sign-in could not be completed. Please try again.';
  }
  return notice;
}
