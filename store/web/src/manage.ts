import { packagePath, type Actions } from './actions';
import { latestInChannel } from './channels';
import { element, routeLink } from './dom';
import { renderPeople } from './people';
import { renderPublisher, renderReleaseForm } from './publishing';
import type { Applet, PackageDetails } from './types';

export function renderPackage(
  pkg: PackageDetails,
  applet: Applet | undefined,
  actions: Actions,
): HTMLElement {
  document.title = `${pkg.applet_id} settings · Apell Store`;
  const root = element('div', 'account-page');
  root.append(routeLink('← My packages', '/account', 'back-link'));
  const heading = element('header', 'manage-heading');
  const copy = element('div');
  copy.append(
    element('p', 'eyebrow', pkg.role === 'owner' ? 'Your package' : 'Maintained package'),
    element('h1', '', pkg.applet_id),
  );
  heading.append(copy);
  if (applet)
    heading.append(
      routeLink(
        'View in Store ↗',
        `/applets/${encodeURIComponent(pkg.applet_id)}`,
        'button secondary',
      ),
    );
  root.append(heading);
  const selected = new URL(location.href).searchParams.get('tab');
  const tab = selected === 'people' || selected === 'publishing' ? selected : 'releases';
  const navigation = element('nav', 'manage-tabs');
  navigation.setAttribute('aria-label', 'Package settings');
  for (const [key, label] of [
    ['releases', 'Releases'],
    ['publishing', 'Publishing'],
    ['people', 'People'],
  ]) {
    const item = routeLink(label, `${packagePath(pkg.applet_id)}?tab=${key}`);
    if (key === tab) item.setAttribute('aria-current', 'page');
    navigation.append(item);
  }
  root.append(navigation);
  if (tab === 'people') root.append(renderPeople(pkg, actions));
  else if (tab === 'publishing') root.append(renderPublisher(pkg, actions));
  else {
    if (applet) {
      const stable = latestInChannel(applet, 'stable');
      const nightly = latestInChannel(applet, 'nightly');
      root.append(
        element(
          'p',
          'release-summary',
          `Stable: ${stable?.version ?? 'None'} · Nightly: ${nightly?.version ?? 'None'}. Published versions are immutable.`,
        ),
      );
    } else
      root.append(element('p', 'release-summary', 'Registered and ready for its first release.'));
    root.append(renderReleaseForm(pkg, applet, actions));
  }
  return root;
}
