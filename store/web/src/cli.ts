import { api, ApiError } from './api';
import { mutate, perform, type Actions } from './actions';
import { cliCodeFromSearch, cliLoginUrl, cliRequest, normalizeCliCode } from './cli-request';
import {
  button,
  element,
  inputField,
  link,
  routeLink,
  section,
  showError,
  statusMessage,
  submitButton,
} from './dom';
import type { CliCredential, Session } from './types';

function approvalHeading(): HTMLElement {
  const root = element('div', 'account-page cli-approval');
  root.append(routeLink('← My packages', '/account', 'back-link'));
  const header = element('header', 'manage-heading');
  const copy = element('div');
  copy.append(element('p', 'eyebrow', 'Apell CLI'), element('h1', '', 'Connect your terminal'));
  header.append(copy);
  root.append(header);
  return root;
}

function verificationCode(code: string): HTMLElement {
  const box = element('div', 'cli-verification');
  box.append(element('span', 'field-hint', 'Verification code'), element('code', '', code));
  return box;
}

function codeEntry(actions: Actions): HTMLElement {
  const panel = section(
    'Enter your terminal code',
    'Run apell login, then enter its verification code.',
  );
  const form = element('form', 'manage-form');
  const field = inputField(form, 'user_code', 'Verification code', {
    required: true,
    placeholder: 'ABCDE-FGHJK',
    hint: 'Only continue with a request you started in your own terminal.',
  }) as HTMLInputElement;
  field.maxLength = 11;
  field.autocomplete = 'off';
  field.autocapitalize = 'characters';
  field.spellcheck = false;
  const status = statusMessage();
  form.append(submitButton('Continue'), status);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    const code = normalizeCliCode(field.value);
    if (!code) {
      status.textContent = 'Enter the ten-character code shown in your terminal.';
      status.classList.add('error');
      return;
    }
    actions.navigate(`/account/cli?code=${encodeURIComponent(code)}`);
  });
  panel.append(form);
  return panel;
}

function signedOut(session: Session, code: string): HTMLElement {
  const panel = section('Sign in to continue');
  panel.append(verificationCode(code));
  if (session.login_available) {
    panel.append(
      element('p', 'muted', 'Sign in with GitHub, then review and approve this terminal request.'),
      link('Continue with GitHub', cliLoginUrl(code), 'button primary'),
    );
  } else {
    panel.append(
      element(
        'p',
        'notice',
        'GitHub sign-in is unavailable on this Store. Try CLI login again later.',
      ),
      routeLink('Browse applets →', '/', 'text-link'),
    );
  }
  return panel;
}

export function renderCliApproval(actions: Actions, signal: AbortSignal): HTMLElement {
  document.title = 'Connect CLI · Apell Store';
  const root = approvalHeading();
  const code = cliCodeFromSearch(location.search);
  if (!code) {
    root.append(codeEntry(actions));
    return root;
  }
  if (!actions.session.user) {
    root.append(signedOut(actions.session, code));
    return root;
  }
  const panel = section('Review this request');
  const loading = statusMessage();
  loading.textContent = 'Loading terminal request…';
  panel.append(loading);
  root.append(panel);
  const endpoint = `/api/v1/account/cli/requests/${encodeURIComponent(code)}`;
  void api<unknown>(endpoint, { signal })
    .then((response) => {
      if (signal.aborted || !root.isConnected) return;
      const request = cliRequest(response, code);
      panel.replaceChildren();
      if (request.status !== 'pending') {
        showCompletion(panel, request.status === 'approved');
        return;
      }
      if (request.expires_at * 1000 <= Date.now()) {
        panel.append(
          element('h2', '', 'This request has expired'),
          element('p', 'muted', 'Return to your terminal and run apell login again.'),
        );
        return;
      }
      panel.append(
        element('h2', '', 'Approve CLI access?'),
        element(
          'p',
          'notice',
          'Only approve if you started this request and the code matches your terminal.',
        ),
        verificationCode(code),
      );
      const account = element('dl', 'cli-details');
      account.append(
        element('dt', '', 'GitHub account'),
        element('dd', '', actions.session.user!.github_login),
        element('dt', '', 'Store'),
        element('dd', '', location.origin),
      );
      panel.append(
        account,
        element(
          'p',
          'muted',
          'This CLI can register packages and publish or manage packages you own or maintain, including their GitHub publishing connections and maintainers.',
        ),
        element(
          'p',
          'field-hint',
          `Request expires ${new Date(request.expires_at * 1000).toLocaleTimeString()}. You can revoke CLI access from My packages.`,
        ),
      );
      const controls = element('div', 'action-row');
      const status = statusMessage();
      const decide = (approved: boolean) => {
        void perform(
          panel,
          status,
          () =>
            mutate(actions, `${endpoint}/${approved ? 'approve' : 'deny'}`, 'POST').catch(
              (error: unknown) => {
                // Reload this approval route so signing in again keeps its code.
                if (error instanceof ApiError && error.status === 401) actions.refresh();
                throw error;
              },
            ),
          () => showCompletion(panel, approved),
        );
      };
      controls.append(
        button('Approve CLI access', () => decide(true), 'button primary'),
        button('Deny', () => decide(false), 'button secondary'),
      );
      panel.append(controls, status);
    })
    .catch((error: unknown) => {
      if (signal.aborted || !root.isConnected) return;
      panel.replaceChildren(element('h2', '', 'Couldn’t load this request'));
      if (error instanceof ApiError && error.status === 401) {
        panel.append(
          element(
            'p',
            'notice error',
            'Your session has expired. Sign in again to review this request.',
          ),
          link('Continue with GitHub', cliLoginUrl(code), 'button primary'),
        );
      } else if (error instanceof ApiError && [400, 404, 410].includes(error.status)) {
        panel.append(
          element(
            'p',
            'muted',
            'This code is invalid or has expired. Run apell login again in your terminal.',
          ),
        );
      } else {
        const status = statusMessage();
        showError(status, error);
        panel.append(status, button('Try again', actions.refresh, 'button secondary'));
      }
    });
  return root;
}

function showCompletion(panel: HTMLElement, approved: boolean): void {
  panel.replaceChildren(
    element('h2', '', approved ? 'CLI access approved' : 'Request denied'),
    element(
      'p',
      'muted',
      approved
        ? 'Return to your terminal to finish signing in.'
        : 'No CLI access was granted. You can close this page.',
    ),
    routeLink('My packages →', '/account', 'text-link'),
  );
}

export function renderCliCredentials(actions: Actions, signal: AbortSignal): HTMLElement {
  const panel = section(
    'CLI access',
    'Review and revoke terminals authorized to use your Store account.',
  );
  panel.append(routeLink('Connect a terminal →', '/account/cli', 'text-link'));
  const content = element('div');
  const loading = statusMessage();
  loading.textContent = 'Loading CLI access…';
  content.append(loading);
  panel.append(content);
  void api<{ credentials: CliCredential[] }>('/api/v1/account/cli/credentials', { signal })
    .then(({ credentials }) => {
      if (signal.aborted || !panel.isConnected) return;
      content.replaceChildren();
      if (!credentials.length) {
        content.append(element('p', 'muted', 'No CLI credentials are active.'));
        return;
      }
      const list = element('ul', 'manage-list');
      for (const credential of credentials) {
        const row = element('li', 'member-row');
        const copy = element('div', 'member-copy');
        copy.append(
          element('strong', '', `Apell CLI · ${credential.id.slice(-8)}`),
          element(
            'p',
            'field-hint',
            `Connected ${new Date(credential.created_at * 1000).toLocaleString()}`,
          ),
          element(
            'p',
            'field-hint',
            `Expires ${new Date(credential.expires_at * 1000).toLocaleString()}`,
          ),
        );
        const status = statusMessage();
        const revoke = button(
          'Revoke',
          () => {
            void perform(
              row,
              status,
              () =>
                mutate(
                  actions,
                  `/api/v1/account/cli/credentials/${encodeURIComponent(credential.id)}`,
                  'DELETE',
                ),
              () => actions.refresh(),
            );
          },
          'button secondary',
        );
        row.append(copy, revoke, status);
        list.append(row);
      }
      content.append(list);
    })
    .catch((error: unknown) => {
      if (signal.aborted || !panel.isConnected) return;
      showError(loading, error);
    });
  return panel;
}
