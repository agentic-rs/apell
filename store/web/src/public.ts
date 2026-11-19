import type { Applet, AppletDetails, Release } from './types';
import { button, element, link } from './dom';
import {
  channel_labels,
  availableChannel,
  catalogRelease,
  channelPath,
  getReleaseChannel,
  latestInChannel,
  selectedChannel,
  setChannel,
  type ReleaseChannel,
} from './channels';

function localUrl(value: string): string | undefined {
  if (!value.startsWith('/') || value.startsWith('//')) return undefined;
  const url = new URL(value, location.origin);
  return url.origin === location.origin ? url.href : undefined;
}

function projectUrl(value: string | null): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    if (url.protocol === 'https:' && !url.username && !url.password) return url.href;
  } catch {
    // Invalid optional metadata does not prevent downloading a valid release.
  }
  return undefined;
}

function icon(applet: Applet, class_name = ''): HTMLElement {
  const frame = element('span', `applet-icon ${class_name}`);
  frame.setAttribute('aria-hidden', 'true');
  frame.textContent = Array.from(applet.name.trim())[0]?.toLocaleUpperCase() || 'A';
  const url = applet.icon_url ? localUrl(applet.icon_url) : undefined;
  if (url) {
    const image = element('img');
    image.alt = '';
    image.width = 96;
    image.height = 96;
    image.src = url;
    image.addEventListener('error', () => image.remove(), { once: true });
    frame.append(image);
  }
  return frame;
}

function badge(release: Release): HTMLElement {
  return element(
    'span',
    `badge${release.prerelease ? ' nightly' : ''}`,
    channel_labels[getReleaseChannel(release)],
  );
}

function channelPicker(onSelect: (channel: ReleaseChannel) => void) {
  const group = element('div', 'channel-picker');
  group.setAttribute('role', 'group');
  group.setAttribute('aria-label', 'Release channel');
  const stable = button(channel_labels.stable, () => onSelect('stable'));
  const nightly = button(channel_labels.nightly, () => onSelect('nightly'));
  group.append(stable, nightly);
  return {
    group,
    sync: (channel: ReleaseChannel, releases: readonly Release[]) => {
      stable.setAttribute('aria-pressed', String(channel === 'stable'));
      nightly.setAttribute('aria-pressed', String(channel === 'nightly'));
      stable.disabled = !releases.some((release) => getReleaseChannel(release) === 'stable');
      nightly.disabled = !releases.some((release) => getReleaseChannel(release) === 'nightly');
    },
  };
}

function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function dateLabel(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? 'Date unavailable'
    : new Intl.DateTimeFormat(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
      }).format(date);
}

function state(title: string, description: string): HTMLElement {
  const section = element('section', 'empty-state');
  section.append(element('h2', '', title), element('p', '', description));
  return section;
}

export interface PublicView<T> {
  update: (data: T) => void;
  syncUrl: () => void;
}

export function renderCatalog(
  main: HTMLElement,
  applets: Applet[],
): PublicView<{ applets: Applet[] }> {
  document.title = 'Apell Store';
  const introduction = element('section', 'catalog-intro');
  const copy = element('div');
  copy.append(
    element('p', 'eyebrow', 'The applet collection'),
    element('h1', '', 'Find your next little app.'),
    element('p', 'lead', 'Download an applet, open it in Apell, and make it yours.'),
  );
  introduction.append(copy);
  const search_form = element('form', 'search');
  search_form.setAttribute('role', 'search');
  const search_label = element('label', '', 'Search applets');
  search_label.htmlFor = 'search';
  const search_input = element('input');
  search_input.type = 'search';
  search_input.id = 'search';
  search_input.placeholder = 'Search by name or description';
  search_input.autocomplete = 'off';
  search_input.value = new URL(location.href).searchParams.get('q') ?? '';
  search_form.append(search_label, search_input);
  const catalog = element('section', 'catalog');
  catalog.setAttribute('aria-label', 'Applets');
  const count = element('p', 'result-count');
  count.setAttribute('role', 'status');
  const grid = element('ul', 'applet-grid');
  const search_state = element('div');
  function filter(): void {
    const query = search_input.value.trim().toLocaleLowerCase();
    const available = applets.map((applet) => ({ applet, release: catalogRelease(applet) }));
    const matches = available.filter(({ applet }) =>
      `${applet.name} ${applet.description}`.toLocaleLowerCase().includes(query),
    );
    count.textContent = `${matches.length} ${matches.length === 1 ? 'applet' : 'applets'}`;
    grid.replaceChildren(...matches.map(({ applet, release }) => renderCard(applet, release)));
    search_state.replaceChildren();
    if (!matches.length) {
      search_state.append(
        available.length
          ? state('Nothing here matches yet.', 'Try another name or a shorter search.')
          : state(
              'No applets published yet.',
              'Published applets will appear here, ready to download.',
            ),
      );
    }
    const url = new URL(location.href);
    if (search_input.value) url.searchParams.set('q', search_input.value);
    else url.searchParams.delete('q');
    url.searchParams.delete('channel');
    history.replaceState(history.state, '', url);
  }
  search_input.addEventListener('input', filter);
  search_form.addEventListener('submit', (event) => event.preventDefault());
  catalog.append(search_form, count, grid, search_state);
  const instructions = element('aside', 'install-tip');
  instructions.append(
    element('strong', '', 'New to applets?'),
    element(
      'p',
      '',
      'Download an .apel file. In Apell, tap +, then “Install an .apel file” to add it to your library.',
    ),
  );
  main.replaceChildren(introduction, catalog, instructions);
  filter();
  return {
    update: (data) => {
      applets = data.applets;
      filter();
    },
    syncUrl: () => {
      const url = new URL(location.href);
      search_input.value = url.searchParams.get('q') ?? '';
      filter();
    },
  };
}

function renderCard(applet: Applet, release: Release): HTMLLIElement {
  const item = element('li');
  const card = link(
    '',
    channelPath(`/applets/${encodeURIComponent(applet.applet_id)}`, getReleaseChannel(release)),
    'applet-card',
  );
  card.dataset.route = '';
  const copy = element('div', 'card-copy');
  const heading = element('div', 'card-heading');
  heading.append(element('h2', '', applet.name), badge(release));
  copy.append(heading, element('p', 'card-description', applet.description));
  const footer = element('div', 'card-footer');
  footer.append(
    element('span', 'version', `v${release.version}`),
    element('span', '', sizeLabel(release.size_bytes)),
  );
  card.append(icon(applet), copy, footer);
  item.append(card);
  return item;
}

const permission_labels: Record<string, string> = {
  sqlite: 'Local database',
  llm: 'AI models',
  resources: 'Downloaded resources',
  device: 'Device information',
  http: 'HTTP requests',
  fs: 'Private files',
  config: 'Preferences',
  websocket: 'Live connections',
  secrets: 'Secure storage',
  crypto: 'Secure randomness',
  runtime: 'Background tasks',
  audio: 'Microphone and audio',
  speech: 'On-device transcription',
  camera: 'Camera',
  location: 'Location',
  photos: 'Photo library',
};

export function renderDetails(
  main: HTMLElement,
  details: AppletDetails,
): PublicView<AppletDetails> {
  let { applet, releases } = details;
  let channel = selectedChannel(new URL(location.href));
  document.title = `${applet.name} · Apell Store`;
  const back = link('← All applets', '/', 'back-link');
  back.dataset.route = '';
  const heading = element('section', 'detail-heading');
  const title_group = element('div', 'detail-title');
  const title_text = element('div');
  const title = element('h1', '', applet.name);
  const artwork = element('span');
  artwork.append(icon(applet, 'large'));
  title_text.append(element('p', 'eyebrow', 'Apell applet'), title);
  title_group.append(artwork, title_text);
  heading.append(title_group);
  const content = element('div', 'detail-layout');
  const article = element('article', 'detail-copy');
  const description = element('p', 'detail-description', applet.description);
  const project = element('div');
  const project_url = projectUrl(applet.project_url);
  if (project_url) project.append(link('Visit project ↗', project_url, 'text-link'));
  article.append(description, project);
  const permissions = element('section', 'detail-section');
  const permission_list = element('ul', 'permission-list');
  const permission_note = element('p', 'muted', 'Apell manages access for each installed applet.');
  permissions.append(element('h2', '', 'Declared access'), permission_note, permission_list);
  const notes = element('section', 'detail-section');
  const release_notes = element('p', 'release-notes');
  notes.append(element('h2', '', 'Release notes'), release_notes);
  article.append(notes, permissions);

  const install = element('aside', 'download-panel');
  install.setAttribute('aria-label', 'Download applet');
  const picker = channelPicker((next) => updateChannel(next));
  const version_label = element('label', '', 'Version');
  version_label.htmlFor = 'release';
  const select = element('select');
  select.id = 'release';
  const release_status = element('div', 'release-status');
  const release_info = element('p', 'muted');
  const download = link('Download .apel ↓', '#', 'button primary');
  const install_steps = element('div', 'install-steps');
  install_steps.append(element('h3', '', 'Open it in Apell'));
  const steps = element('ol');
  steps.append(
    element('li', '', 'Download the .apel file.'),
    element('li', '', 'In Apell, tap + → Install an .apel file.'),
    element('li', '', 'Choose the file, then open its icon.'),
  );
  install_steps.append(steps);
  const integrity = element('details', 'integrity');
  integrity.append(element('summary', '', 'Verify this download'));
  const checksum = element('code');
  const copy_status = element('p', 'copy-status');
  copy_status.setAttribute('role', 'status');
  const copy = button(
    'Copy SHA-256',
    () => {
      void navigator.clipboard
        ?.writeText(checksum.textContent ?? '')
        .then(() => {
          copy_status.textContent = 'Checksum copied.';
        })
        .catch(() => {
          copy_status.textContent = 'Select the checksum to copy it.';
        });
      if (!navigator.clipboard) copy_status.textContent = 'Select the checksum to copy it.';
    },
    'copy-button',
  );
  integrity.append(
    element('p', 'muted', 'SHA-256 identifies the exact published file.'),
    checksum,
    copy,
    copy_status,
  );
  install.append(
    picker.group,
    version_label,
    select,
    release_status,
    release_info,
    download,
    install_steps,
    integrity,
  );

  function updateChannel(next: ReleaseChannel, version?: string): void {
    channel = availableChannel(next, releases);
    picker.sync(channel, releases);
    const url = new URL(location.href);
    setChannel(url, channel);
    history.replaceState(history.state, '', url);
    back.href = '/';

    const available = releases.filter((release) => getReleaseChannel(release) === channel);
    const latest = latestInChannel(applet, channel, releases);
    select.replaceChildren(
      ...available.map((release) => {
        const label = `v${release.version}${release.version === latest?.version ? ' · Latest' : ''}`;
        const option = element('option', '', label);
        option.value = release.version;
        return option;
      }),
    );
    select.disabled = available.length === 0;
    if (select.disabled) select.append(element('option', '', 'No published versions'));
    else
      select.value = available.some((release) => release.version === version)
        ? version!
        : (latest?.version ?? available[0].version);
    selectRelease();
  }

  function selectRelease(): void {
    const url = new URL(location.href);
    if (select.value && !select.disabled) url.searchParams.set('version', select.value);
    else url.searchParams.delete('version');
    history.replaceState(history.state, '', url);
    const release = releases.find((candidate) => candidate.version === select.value);
    copy_status.textContent = '';
    integrity.hidden = !release;
    permissions.hidden = !release;
    if (!release) {
      release_status.replaceChildren();
      release_info.textContent = `No ${channel_labels[channel]} releases are available.`;
      download.removeAttribute('href');
      download.removeAttribute('download');
      download.setAttribute('aria-disabled', 'true');
      release_notes.textContent = 'Select a published version to view its release notes.';
      permission_list.replaceChildren();
      checksum.textContent = '';
      return;
    }
    release_status.replaceChildren(badge(release));
    if (release.prerelease)
      release_status.append(element('p', '', 'Nightly builds may be less reliable.'));
    release_info.textContent = `${sizeLabel(release.size_bytes)} · ${dateLabel(release.published_at)}`;
    const download_url = localUrl(release.download_url);
    if (download_url) {
      download.href = download_url;
      download.download = `${applet.applet_id}-${release.version}.apel`;
      download.removeAttribute('aria-disabled');
    } else {
      download.removeAttribute('href');
      download.setAttribute('aria-disabled', 'true');
    }
    release_notes.textContent = release.release_notes || 'No release notes for this version.';
    checksum.textContent = release.sha256;
    permission_list.replaceChildren(
      ...release.permissions.map((permission) =>
        element('li', '', permission_labels[permission] ?? permission),
      ),
    );
    permission_list.append(
      element(
        'li',
        release.network ? '' : 'disabled-access',
        release.network ? 'Network access' : 'Network disabled',
      ),
    );
  }
  select.addEventListener('change', selectRelease);
  updateChannel(channel, new URL(location.href).searchParams.get('version') ?? undefined);
  content.append(article, install);

  const history_section = element('section', 'version-history');
  history_section.append(element('h2', '', 'Version history'));
  const history_list = element('ul', 'release-list');
  function updateHistory(): void {
    history_list.replaceChildren();
    for (const release of releases) {
      const row = element('li');
      const title = element('div', 'history-title');
      title.append(element('strong', '', `v${release.version}`), badge(release));
      const info = element('div', 'history-info');
      info.append(
        element('span', '', dateLabel(release.published_at)),
        element('span', '', sizeLabel(release.size_bytes)),
      );
      const choose = button(
        'View version',
        () => {
          updateChannel(getReleaseChannel(release), release.version);
          install.scrollIntoView({ behavior: 'instant', block: 'center' });
          select.focus({ preventScroll: true });
        },
        'version-button',
      );
      choose.setAttribute('aria-label', `View version ${release.version}`);
      row.append(title, info, choose);
      history_list.append(row);
    }
  }
  updateHistory();
  history_section.append(history_list);
  main.replaceChildren(back, heading, content, history_section);
  return {
    update: (data) => {
      const selected = select.value;
      const previous_icon = applet.icon_url;
      ({ applet, releases } = data);
      document.title = `${applet.name} · Apell Store`;
      title.textContent = applet.name;
      description.textContent = applet.description;
      if (previous_icon !== applet.icon_url) artwork.replaceChildren(icon(applet, 'large'));
      const next_project = projectUrl(applet.project_url);
      project.replaceChildren(
        ...(next_project ? [link('Visit project ↗', next_project, 'text-link')] : []),
      );
      updateChannel(channel, selected);
      updateHistory();
    },
    syncUrl: () => {
      const url = new URL(location.href);
      updateChannel(selectedChannel(url), url.searchParams.get('version') ?? undefined);
    },
  };
}
