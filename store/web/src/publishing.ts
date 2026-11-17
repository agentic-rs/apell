import { mutate, packageEndpoint, perform, type Actions } from './actions';
import { button, element, inputField, section, statusMessage, submitButton } from './dom';
import type { Applet, PackageDetails, PublishResult } from './types';

export function renderReleaseForm(
  pkg: PackageDetails,
  applet: Applet | undefined,
  actions: Actions,
): HTMLElement {
  const panel = section(
    'Publish a release',
    'Upload a built .apel file. Its package ID must match this package, and changed content needs a new version.',
  );
  const form = element('form', 'manage-form');
  const package_file = inputField(form, 'package', 'Applet package', {
    type: 'file',
    required: true,
    hint: '.apel format, up to 24 MiB.',
  }) as HTMLInputElement;
  package_file.accept = '.apel';
  const description = inputField(form, 'description', 'Description', {
    value: applet?.description,
    required: true,
    hint: 'A short description shown in the public catalog.',
  });
  const project_url = inputField(form, 'project_url', 'Project website (optional)', {
    type: 'url',
    value: applet?.project_url ?? '',
    placeholder: 'https://…',
  });
  const release_notes = inputField(form, 'release_notes', 'Release notes (optional)', {
    multiline: true,
    hint: 'What changed in this version?',
  });
  const icon = inputField(form, 'icon', 'Listing icon (optional)', {
    type: 'file',
    hint: 'A still PNG, up to 256 KiB. Include it with each new release to keep your listing artwork.',
  }) as HTMLInputElement;
  icon.accept = 'image/png,.png';
  const status = statusMessage();
  form.append(submitButton('Publish release'), status);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    const file = package_file.files?.[0];
    if (!file) return;
    const body = new FormData();
    body.append('package', file);
    body.append(
      'metadata',
      JSON.stringify({
        description: description.value.trim(),
        project_url: project_url.value.trim() || null,
        release_notes: release_notes.value,
      }),
    );
    const image = icon.files?.[0];
    if (image) body.append('icon', image);
    void perform(
      form,
      status,
      async () => {
        if (file.size > 24 * 1024 * 1024)
          throw new Error('Choose an .apel file smaller than 24 MiB.');
        if (image && image.size > 256 * 1024)
          throw new Error('Choose a PNG icon smaller than 256 KiB.');
        return mutate<PublishResult>(
          actions,
          `${packageEndpoint(pkg.applet_id)}/publish`,
          'POST',
          body,
        );
      },
      (result) => {
        package_file.value = '';
        icon.value = '';
        release_notes.value = '';
        status.textContent = `${result.created ? 'Published' : 'Already published'} ${result.applet_id} ${result.version}.`;
        const view = element('a', 'text-link', 'View published applet →');
        view.href = `/applets/${encodeURIComponent(result.applet_id)}`;
        view.dataset.route = '';
        status.append(document.createTextNode(' '), view);
      },
    );
  });
  panel.append(form);
  return panel;
}

export function renderPublisher(pkg: PackageDetails, actions: Actions): HTMLElement {
  const panel = section(
    'GitHub Actions',
    'Allow a trusted workflow to publish this package without storing a publishing secret in GitHub.',
  );
  const current = pkg.publisher;
  const connection = element('div', 'publisher-summary');
  connection.append(
    element('span', `badge${current ? '' : ' neutral'}`, current ? 'Connected' : 'Not connected'),
  );
  if (current) {
    connection.append(
      element('strong', '', current.repository || `Repository ${current.repository_id}`),
    );
    connection.append(
      element('p', 'muted', `${current.workflow} · ${current.allowed_refs.join(', ')}`),
    );
  } else {
    connection.append(
      element('p', 'muted', 'You can publish files directly while CI publishing is disconnected.'),
    );
  }
  panel.append(connection);

  const form = element('form', 'manage-form');
  const repository = inputField(form, 'repository', 'GitHub repository', {
    required: true,
    value: current?.repository ?? '',
    placeholder: 'owner/repository',
    hint: 'Enter the public repository that contains your publishing workflow.',
  });
  repository.autocapitalize = 'none';
  repository.spellcheck = false;
  const workflow = inputField(form, 'workflow', 'Workflow file', {
    required: true,
    value: current?.workflow ?? '.github/workflows/publish.yml',
  });
  workflow.spellcheck = false;
  const refs = inputField(form, 'allowed_refs', 'Allowed Git refs', {
    required: true,
    multiline: true,
    value: current?.allowed_refs.join('\n') ?? 'refs/heads/main',
    hint: 'One exact branch or tag ref per line, for example refs/heads/main. Wildcards are not supported.',
  });
  const events = element('fieldset', 'event-options');
  events.append(element('legend', '', 'Allowed workflow events'));
  const event_inputs = ['push', 'workflow_dispatch', 'release'].map((name) => {
    const label = element('label');
    const checkbox = element('input');
    checkbox.type = 'checkbox';
    checkbox.name = 'event_names';
    checkbox.value = name;
    checkbox.checked = (current?.event_names ?? ['push', 'workflow_dispatch']).includes(name);
    label.append(
      checkbox,
      document.createTextNode(
        name === 'workflow_dispatch' ? 'Manual run' : name === 'push' ? 'Push' : 'Release',
      ),
    );
    events.append(label);
    return checkbox;
  });
  form.append(events);
  const environment = inputField(form, 'environment', 'GitHub environment (optional)', {
    value: current?.environment ?? '',
    hint: 'If set, the publishing job must use this exact GitHub environment.',
  });
  const status = statusMessage();
  form.append(submitButton(current ? 'Save publishing settings' : 'Connect repository'), status);
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    const event_names = event_inputs.filter((input) => input.checked).map((input) => input.value);
    const allowed_refs = refs.value
      .split(/\r?\n/)
      .map((value) => value.trim())
      .filter(Boolean);
    void perform(
      form,
      status,
      () => {
        if (!event_names.length) throw new Error('Choose at least one workflow event.');
        return mutate(actions, `${packageEndpoint(pkg.applet_id)}/publisher`, 'PUT', {
          repository: repository.value.trim(),
          workflow: workflow.value.trim(),
          allowed_refs,
          event_names,
          environment: environment.value.trim() || null,
        });
      },
      () => actions.refresh(),
    );
  });
  panel.append(form);

  if (current) {
    const disconnect = element('div', 'disconnect-row');
    const result = statusMessage();
    disconnect.append(
      button(
        'Disconnect GitHub publishing',
        () => {
          if (
            !window.confirm(
              'Disconnect this workflow? It will lose permission to publish immediately. Existing releases and downloads stay available.',
            )
          )
            return;
          void perform(
            disconnect,
            result,
            () => mutate(actions, `${packageEndpoint(pkg.applet_id)}/publisher`, 'DELETE'),
            () => actions.refresh(),
          );
        },
        'button danger',
      ),
      result,
    );
    panel.append(disconnect);
  }

  const setup = element('details', 'workflow-guide');
  setup.append(element('summary', '', 'Set up the GitHub workflow'));
  const instructions = element('ol');
  instructions.append(
    element(
      'li',
      '',
      `In your repository’s Actions variables, set APELL_STORE_URL to ${location.origin}.`,
    ),
    element(
      'li',
      '',
      'Use a GitHub-hosted runner and grant the publishing job contents: read and id-token: write.',
    ),
    element(
      'li',
      '',
      'Build your applet, then run the publishing command below from the registered workflow and ref.',
    ),
  );
  const command = element('pre');
  command.append(
    element(
      'code',
      '',
      'apell publish artifacts/app.apel \\\n  --store "$APELL_STORE_URL" \\\n  --metadata listing.json \\\n  --icon icon.png \\\n  --auth github-oidc',
    ),
  );
  setup.append(
    instructions,
    command,
    element(
      'p',
      'field-hint',
      'Omit --icon if you do not have a listing icon. A changed release must use a new version.',
    ),
  );
  panel.append(setup);
  return panel;
}
