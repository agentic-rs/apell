import { ApiError } from './api';
import { button, element } from './dom';
import { publicCache, type PublicData } from './public-cache';
import { renderCatalog, renderDetails } from './public';

interface Page {
  path: string;
  value: string;
  update: (data: PublicData) => void;
  syncUrl: () => void;
}
let active: Page | undefined;

export function clearPublicView(): void {
  active = undefined;
}

export async function renderPublicRoute(
  main: HTMLElement,
  path: string,
  isCurrent: () => boolean,
  retry: () => void,
  force = false,
  onDisplay?: () => void,
): Promise<void> {
  function display(data: PublicData): void {
    const value = JSON.stringify(data);
    if (active?.path === path) {
      if (active.value !== value) active.update(data);
      active.value = value;
      return;
    }
    if ('applets' in data) {
      const view = renderCatalog(main, data.applets);
      active = {
        path,
        value,
        syncUrl: view.syncUrl,
        update: (next) => {
          if ('applets' in next) view.update(next);
        },
      };
    } else {
      const view = renderDetails(main, data);
      active = {
        path,
        value,
        syncUrl: view.syncUrl,
        update: (next) => {
          if ('applet' in next) view.update(next);
        },
      };
    }
  }
  const saved = publicCache.read(path);
  if (active?.path === path) active.syncUrl();
  else if (saved) display(saved.data);
  else {
    active = undefined;
    const loading = element('div', 'loading-state', 'Loading applets…');
    loading.setAttribute('role', 'status');
    main.replaceChildren(loading);
    main.setAttribute('aria-busy', 'true');
  }
  if (active?.path === path) {
    main.removeAttribute('aria-busy');
    onDisplay?.();
  }
  try {
    const data = await publicCache.load(path, force);
    if (!isCurrent()) return;
    display(data);
    main.querySelector('[data-cache-status]')?.remove();
    onDisplay?.();
  } catch (error) {
    if (!isCurrent()) return;
    if (error instanceof DOMException && error.name === 'AbortError') return;
    if (active?.path !== path || (error instanceof ApiError && error.status === 404)) throw error;
    main.querySelector('[data-cache-status]')?.remove();
    const note = element('aside', 'cache-status');
    note.dataset.cacheStatus = '';
    note.setAttribute('role', 'status');
    note.append(
      element('p', '', 'Showing saved information. Updates could not be checked.'),
      button('Try again', retry, 'button secondary'),
    );
    main.append(note);
  }
}
