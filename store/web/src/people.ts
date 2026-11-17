import { mutate, packageEndpoint, perform, type Actions } from './actions';
import { button, element, inputField, section, statusMessage, submitButton } from './dom';
import type { Invitation, PackageDetails, User } from './types';

export function renderPeople(pkg: PackageDetails, actions: Actions): HTMLElement {
  const panel = section(
    'People',
    'Every maintainer can publish, change publishing settings, and invite or remove other maintainers.',
  );
  const list = element('ul', 'manage-list');
  list.append(personRow(pkg.owner, true, pkg, actions));
  const maintainers = pkg.maintainers.filter((user) => user.github_id !== pkg.owner.github_id);
  maintainers.forEach((user) => list.append(personRow(user, false, pkg, actions)));
  panel.append(list);

  const form = element('form', 'manage-form compact-form');
  const username = inputField(form, 'github_login', 'Invite a maintainer', {
    required: true,
    placeholder: 'GitHub username',
    hint: 'They will see the invitation after signing in with GitHub.',
  });
  username.autocapitalize = 'none';
  username.spellcheck = false;
  const status = statusMessage();
  form.append(submitButton('Send invitation'), status);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    void perform(
      form,
      status,
      () =>
        mutate(actions, `${packageEndpoint(pkg.applet_id)}/members`, 'POST', {
          github_login: username.value.trim().replace(/^@/, ''),
        }),
      () => actions.refresh(),
    );
  });
  panel.append(form);

  if (pkg.invitations.length) {
    const pending = element('div', 'pending-invitations');
    pending.append(element('h3', '', 'Pending invitations'));
    const invitations = element('ul', 'manage-list');
    pkg.invitations.forEach((invitation) =>
      invitations.append(invitationRow(invitation, pkg, actions)),
    );
    pending.append(invitations);
    panel.append(pending);
  }

  if (pkg.role === 'owner') {
    const transfer = element('div', 'ownership-section');
    transfer.append(
      element('h3', '', 'Transfer ownership'),
      element(
        'p',
        'muted',
        'Choose an existing maintainer. Ownership changes only after they accept your invitation.',
      ),
    );
    if (!maintainers.length) {
      transfer.append(
        element('p', 'field-hint', 'Invite a maintainer before transferring ownership.'),
      );
    } else {
      const transfer_form = element('form', 'manage-form compact-form');
      const label = element('label', '', 'New owner');
      label.htmlFor = 'new_owner';
      const select = element('select');
      select.id = 'new_owner';
      select.name = 'github_id';
      const empty = element('option', '', 'Choose a maintainer');
      empty.value = '';
      select.append(empty);
      select.required = true;
      for (const user of maintainers) {
        const option = element('option', '', user.github_login);
        option.value = String(user.github_id);
        select.append(option);
      }
      const field = element('div', 'form-field');
      field.append(label, select);
      const result = statusMessage();
      transfer_form.append(field, submitButton('Invite new owner'), result);
      transfer_form.addEventListener('submit', (event) => {
        event.preventDefault();
        void perform(
          transfer_form,
          result,
          () =>
            mutate(actions, `${packageEndpoint(pkg.applet_id)}/ownership`, 'POST', {
              github_id: Number(select.value),
            }),
          () => actions.refresh(),
        );
      });
      transfer.append(transfer_form);
    }
    panel.append(transfer);
  }
  return panel;
}

function personRow(
  user: User,
  owner: boolean,
  pkg: PackageDetails,
  actions: Actions,
): HTMLLIElement {
  const row = element('li', 'person-row');
  const copy = element('div', 'member-copy');
  copy.append(
    element('strong', '', user.github_login),
    element('span', 'badge', owner ? 'Owner' : 'Maintainer'),
  );
  row.append(copy);
  if (owner) {
    row.append(element('span', 'field-hint', 'Protected owner'));
  } else {
    const status = statusMessage();
    const self = user.github_id === actions.session.user?.github_id;
    row.append(
      button(
        self ? 'Leave package' : 'Remove',
        () => {
          if (
            !window.confirm(
              self
                ? 'Leave this package? You will lose access to publishing and settings.'
                : `Remove ${user.github_login} as a maintainer?`,
            )
          )
            return;
          void perform(
            row,
            status,
            () =>
              mutate(
                actions,
                `${packageEndpoint(pkg.applet_id)}/members/${user.github_id}`,
                'DELETE',
              ),
            () => (self ? actions.navigate('/account') : actions.refresh()),
          );
        },
        'button secondary',
      ),
      status,
    );
  }
  return row;
}

function invitationRow(
  invitation: Invitation,
  pkg: PackageDetails,
  actions: Actions,
): HTMLLIElement {
  const row = element('li', 'person-row');
  const copy = element('div', 'member-copy');
  copy.append(
    element('strong', '', invitation.target.github_login),
    element(
      'p',
      'muted',
      invitation.kind === 'ownership' ? 'Ownership transfer' : 'Maintainer invitation',
    ),
  );
  row.append(copy);
  if (invitation.kind === 'maintainer' || pkg.role === 'owner') {
    const status = statusMessage();
    row.append(
      button(
        'Cancel invitation',
        () => {
          void perform(
            row,
            status,
            () =>
              mutate(
                actions,
                `${packageEndpoint(pkg.applet_id)}/invitations/${encodeURIComponent(invitation.id)}`,
                'DELETE',
              ),
            () => actions.refresh(),
          );
        },
        'button secondary',
      ),
      status,
    );
  }
  return row;
}
