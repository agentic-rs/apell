import { api } from './api';
import { showError } from './dom';
import type { Session } from './types';

export interface Actions {
  session: Session;
  refresh: () => void;
  navigate: (path: string) => void;
}

export async function perform<T>(
  scope: HTMLElement,
  status: HTMLElement,
  work: () => Promise<T>,
  complete: (value: T) => void,
): Promise<void> {
  if (scope.getAttribute('aria-busy') === 'true') return;
  const controls = [
    ...scope.querySelectorAll<
      HTMLInputElement | HTMLButtonElement | HTMLTextAreaElement | HTMLSelectElement
    >('input, button, textarea, select'),
  ];
  const disabled = controls.map((control) => control.disabled);
  controls.forEach((control) => {
    control.disabled = true;
  });
  scope.setAttribute('aria-busy', 'true');
  status.classList.remove('error');
  status.textContent = 'Working…';
  try {
    const result = await work();
    if (scope.isConnected) complete(result);
  } catch (error) {
    if (scope.isConnected) showError(status, error);
  } finally {
    scope.removeAttribute('aria-busy');
    controls.forEach((control, index) => {
      control.disabled = disabled[index];
    });
  }
}

export function mutate<T>(
  actions: Actions,
  path: string,
  method: 'POST' | 'PUT' | 'DELETE',
  body?: unknown,
): Promise<T> {
  return api<T>(path, { method, body, csrf_token: actions.session.csrf_token });
}

export function packageEndpoint(applet_id: string): string {
  return `/api/v1/account/packages/${encodeURIComponent(applet_id)}`;
}

export function packagePath(applet_id: string): string {
  return `/account/packages/${encodeURIComponent(applet_id)}`;
}
