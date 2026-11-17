import './styles.css';
import { api, ApiError } from './api';
import { renderAccount, renderAccountHeader, renderRegistration, renderSignedOut } from './account';
import type { Actions } from './actions';
import { button, element, routeLink, showError } from './dom';
import { renderPackage } from './manage';
import { renderCatalog, renderDetails } from './public';
import type { AccountPackages, Applet, AppletDetails, PackageDetails, Session } from './types';

const main = document.querySelector<HTMLElement>('#main')!;
let request: AbortController | undefined;
let navigation_id = 0;
let session: Session | undefined;

function navigate(path: string): void {
  history.pushState(null, '', path);
  window.scrollTo(0, 0);
  void renderRoute(true);
}

function renderLoading(): void {
  const loading = element('div', 'loading-state', 'Loading…');
  loading.setAttribute('role', 'status');
  main.replaceChildren(loading);
  main.setAttribute('aria-busy', 'true');
}

async function signOut(): Promise<void> {
  if (!session?.csrf_token) return;
  const nav = document.querySelector<HTMLElement>('[data-account-nav]');
  if (nav?.getAttribute('aria-busy') === 'true') return;
  nav?.setAttribute('aria-busy', 'true');
  try {
    await api('/api/v1/auth/logout', { method: 'POST', csrf_token: session.csrf_token });
    session = undefined;
    navigate('/account');
  } catch (error) {
    const status = element('p', 'notice error');
    status.setAttribute('role', 'alert');
    showError(status, error);
    main.prepend(status);
  } finally {
    nav?.removeAttribute('aria-busy');
  }
}

async function renderRoute(focus = false): Promise<void> {
  request?.abort();
  request = new AbortController();
  const signal = request.signal;
  const current_id = ++navigation_id;
  renderLoading();
  if (focus) main.focus({ preventScroll: true });
  const auth_request = api<Session>('/api/v1/auth/me', { signal }).catch(() => undefined);
  try {
    const path = location.pathname.replace(/\/$/, '') || '/';
    if (path === '/' || path.startsWith('/applets/')) {
      renderAccountHeader(session, () => void signOut());
      // Public browsing must remain usable if the login service is unavailable.
      void auth_request.then((auth) => {
        if (current_id !== navigation_id) return;
        session = auth;
        renderAccountHeader(session, () => void signOut());
      });
      if (path === '/') {
        const response = await api<{ applets: Applet[] }>('/api/v1/applets', { signal });
        if (current_id !== navigation_id) return;
        renderCatalog(main, response.applets);
      } else {
        const match = /^\/applets\/([a-z0-9_-]+)$/.exec(path);
        if (!match) throw new Error('This page could not be found.');
        const response = await api<AppletDetails>(
          `/api/v1/applets/${encodeURIComponent(match[1])}`,
          { signal },
        );
        if (current_id !== navigation_id) return;
        renderDetails(main, response);
      }
    } else if (
      path === '/account' ||
      path === '/account/new' ||
      /^\/account\/packages\/[a-z0-9_-]+$/.test(path)
    ) {
      const auth = await auth_request;
      if (current_id !== navigation_id) return;
      if (!auth) throw new Error('Could not check your session. Please try again.');
      session = auth;
      renderAccountHeader(session, () => void signOut());
      if (!session.user) {
        main.replaceChildren(renderSignedOut(session));
        return;
      }
      const actions: Actions = {
        session,
        refresh: () => {
          if (current_id === navigation_id) void renderRoute();
        },
        navigate: (destination) => {
          if (current_id === navigation_id) navigate(destination);
        },
      };
      if (path === '/account/new') {
        main.replaceChildren(renderRegistration(actions));
      } else if (path === '/account') {
        const data = await api<AccountPackages>('/api/v1/account/packages', { signal });
        if (current_id !== navigation_id) return;
        main.replaceChildren(renderAccount(data, actions));
      } else {
        const id = path.split('/').at(-1)!;
        const pkg = await api<PackageDetails>(
          `/api/v1/account/packages/${encodeURIComponent(id)}`,
          { signal },
        );
        let applet: Applet | undefined;
        try {
          applet = (
            await api<AppletDetails>(`/api/v1/applets/${encodeURIComponent(id)}`, { signal })
          ).applet;
        } catch (error) {
          if (!(error instanceof ApiError && error.status === 404)) throw error;
        }
        if (current_id !== navigation_id) return;
        main.replaceChildren(renderPackage(pkg, applet, actions));
      }
    } else {
      throw new Error('This page could not be found.');
    }
  } catch (error) {
    if (current_id !== navigation_id) return;
    const state = element('section', 'empty-state');
    const message = element('p');
    showError(message, error);
    state.append(
      element('h2', '', 'Couldn’t load this page.'),
      message,
      button('Try again', () => void renderRoute(), 'button secondary'),
    );
    main.replaceChildren(routeLink('← All applets', '/', 'back-link'), state);
  } finally {
    if (current_id === navigation_id) main.removeAttribute('aria-busy');
  }
}

document.addEventListener('click', (event) => {
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.metaKey ||
    event.ctrlKey ||
    event.shiftKey ||
    event.altKey
  )
    return;
  const target =
    event.target instanceof Element
      ? event.target.closest<HTMLAnchorElement>('a[data-route]')
      : null;
  if (
    !target ||
    target.origin !== location.origin ||
    target.target ||
    target.hasAttribute('download')
  )
    return;
  event.preventDefault();
  navigate(target.href);
});
window.addEventListener('popstate', () => void renderRoute());
const publishing_link = document.querySelector<HTMLAnchorElement>('.site-footer a');
if (publishing_link) {
  publishing_link.href = '/account';
  publishing_link.dataset.route = '';
  publishing_link.textContent = 'Publish an applet →';
}
void renderRoute();
