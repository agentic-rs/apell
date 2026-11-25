import { mutate, packagePath, perform, type Actions } from './actions';
import { renderCliCredentials } from './cli';
import {
  button,
  element,
  inputField,
  link,
  routeLink,
  section,
  statusMessage,
  submitButton,
} from './dom';
import { accountSecurityLink, signInNotice, signInOption } from './sign-in';
import type { AccountPackages, Invitation, PackageDetails, Session } from './types';

export function renderAccountHeader(session: Session | undefined, onLogout: () => void): void {
  const nav = document.querySelector<HTMLElement>('.site-header nav');
  if (!nav) return;
  nav.querySelector('[data-account-nav]')?.remove();
  const group = element('span', 'account-nav');
  group.dataset.accountNav = '';
  if (session?.user) {
    const account = routeLink('My packages', '/account');
    account.title = `Signed in as ${session.user.github_login}`;
    group.append(account, button('Sign out', onLogout, 'nav-button'));
  } else {
    group.append(
      routeLink(session?.login_available === false ? 'Publishing' : 'Sign in', '/account'),
    );
  }
  nav.append(group);
}

export function renderSignedOut(session: Session): HTMLElement {
  document.title = 'Publish applets · Apell Store';
  const root = element('section', 'account-welcome');
  root.append(
    element('h1', '', 'Publish applets'),
    element(
      'p',
      'lead',
      'Sign in to register packages, invite maintainers, and connect your publishing workflow.',
    ),
  );
  const notice = signInNotice(session);
  if (notice) root.append(notice);
  const sign_in = signInOption(session);
  if (sign_in) {
    if (sign_in.provider === 'account' && !notice) {
      root.append(
        element(
          'p',
          'muted',
          'Sign in with your usual Apell Account method. On your first Store sign-in, you may be asked to link GitHub in Account Security.',
        ),
      );
    }
    root.append(link(sign_in.label, sign_in.url, 'button primary'));
  } else {
    root.append(element('p', 'notice', 'Sign-in is currently unavailable on this Store.'));
  }
  root.append(element('p', 'muted', 'Anyone can browse and download applets without signing in.'));
  root.append(routeLink('Browse applets →', '/', 'text-link'));
  return root;
}

export function renderAccount(data: AccountPackages, actions: Actions): HTMLElement {
  document.title = 'My packages · Apell Store';
  const root = element('div', 'account-page');
  const heading = element('header', 'manage-heading');
  const copy = element('div');
  copy.append(
    element('p', 'eyebrow', `GitHub · ${actions.session.user!.github_login}`),
    element('h1', '', 'My packages'),
  );
  const controls = element('div', 'action-row');
  controls.append(
    routeLink('Register package', '/account/new', 'button primary'),
    routeLink('Account & access', '/account/access', 'button secondary'),
  );
  heading.append(copy, controls);
  root.append(heading);
  if (data.invitations.length) {
    const invitations = section('Invitations');
    const list = element('ul', 'manage-list');
    data.invitations.forEach((invitation) => list.append(renderInvitation(invitation, actions)));
    invitations.append(list);
    root.append(invitations);
  }
  if (!data.packages.length) {
    const empty = section(
      'Your first applet starts here.',
      'Register its package ID, then publish an .apel file or connect GitHub Actions.',
    );
    root.append(empty);
  } else {
    const list = element('ul', 'managed-packages');
    for (const pkg of data.packages) {
      const item = element('li');
      const card = routeLink('', packagePath(pkg.applet_id), 'managed-package');
      const identity = element('div');
      identity.append(
        element('h2', '', pkg.applet_id),
        element('p', 'muted', pkg.published ? 'Published' : 'Awaiting first release'),
      );
      card.append(
        identity,
        element('span', 'badge', pkg.role === 'owner' ? 'Owner' : 'Maintainer'),
        element('span', 'manage-arrow', '→'),
      );
      item.append(card);
      list.append(item);
    }
    root.append(list);
  }
  return root;
}

export function renderAccountAccess(actions: Actions, signal: AbortSignal): HTMLElement {
  document.title = 'Account & access · Apell Store';
  const root = element('div', 'account-page');
  const heading = element('header', 'manage-heading');
  heading.append(element('h1', '', 'Account & access'));
  root.append(routeLink('← My packages', '/account', 'back-link'), heading);

  const identity = section('Publishing identity');
  const github = element('dl', 'account-identity');
  github.append(
    element('dt', '', 'GitHub'),
    element('dd', '', `@${actions.session.user!.github_login}`),
  );
  identity.append(github);
  if (actions.session.login_provider === 'account') {
    identity.append(
      element('p', 'muted', 'Manage your sign-in methods and linked accounts in Apell Account.'),
      accountSecurityLink(),
    );
  }
  root.append(identity, renderCliCredentials(actions, signal));
  return root;
}

function renderInvitation(invitation: Invitation, actions: Actions): HTMLLIElement {
  const row = element('li', 'invitation-row');
  const copy = element('div', 'member-copy');
  copy.append(
    element('strong', '', invitation.applet_id),
    element(
      'p',
      'muted',
      `${invitation.invited_by.github_login} invited you to ${invitation.kind === 'ownership' ? 'become the owner' : 'join as a maintainer'}.`,
    ),
  );
  const expiration = new Date(invitation.expires_at * 1000);
  copy.append(element('p', 'field-hint', `Expires ${expiration.toLocaleDateString()}`));
  const controls = element('div', 'action-row');
  const status = statusMessage();
  const endpoint = `/api/v1/account/invitations/${encodeURIComponent(invitation.id)}`;
  controls.append(
    button(
      invitation.kind === 'ownership' ? 'Accept ownership' : 'Accept invitation',
      () => {
        void perform(
          row,
          status,
          () => mutate(actions, `${endpoint}/accept`, 'POST'),
          () => actions.refresh(),
        );
      },
      'button primary',
    ),
    button(
      'Decline',
      () => {
        void perform(
          row,
          status,
          () => mutate(actions, endpoint, 'DELETE'),
          () => actions.refresh(),
        );
      },
      'button secondary',
    ),
  );
  row.append(copy, controls, status);
  return row;
}

export function renderRegistration(actions: Actions): HTMLElement {
  document.title = 'Register package · Apell Store';
  const root = element('div', 'account-page');
  root.append(routeLink('← My packages', '/account', 'back-link'));
  const heading = element('header', 'manage-heading');
  heading.append(element('h1', '', 'Register a package'));
  const panel = section(
    'Choose its identity',
    'You will own the package and can invite other people to maintain it.',
  );
  const form = element('form', 'manage-form');
  const id = inputField(form, 'applet_id', 'Package ID', {
    required: true,
    placeholder: 'my-applet',
    hint: 'Use the same applet_id in manifest.json. This ID is permanent: 1–80 lowercase letters, numbers, hyphens, or underscores.',
  }) as HTMLInputElement;
  id.pattern = '[a-z0-9_\\-]{1,80}';
  id.maxLength = 80;
  id.autocomplete = 'off';
  id.autocapitalize = 'none';
  id.spellcheck = false;
  const status = statusMessage();
  form.append(submitButton('Register package'), status);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    void perform(
      form,
      status,
      () =>
        mutate<PackageDetails>(actions, '/api/v1/account/packages', 'POST', {
          applet_id: id.value.trim(),
        }),
      (pkg) => actions.navigate(packagePath(pkg.applet_id)),
    );
  });
  panel.append(
    form,
    element(
      'p',
      'field-hint',
      'Your package appears in the public catalog after its first release.',
    ),
  );
  root.append(heading, panel);
  return root;
}
