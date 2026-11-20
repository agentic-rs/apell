import { ApiError } from './api';

export function element<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  class_name = '',
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (class_name) node.className = class_name;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function link(text: string, href: string, class_name = ''): HTMLAnchorElement {
  const node = element('a', class_name, text);
  node.href = href;
  return node;
}

export function routeLink(text: string, href: string, class_name = ''): HTMLAnchorElement {
  const node = link(text, href, class_name);
  node.dataset.route = '';
  return node;
}

export function button(text: string, onClick: () => void, class_name = ''): HTMLButtonElement {
  const node = element('button', class_name, text);
  node.type = 'button';
  node.addEventListener('click', onClick);
  return node;
}

export function section(title: string, description?: string): HTMLElement {
  const node = element('section', 'manage-section');
  node.append(element('h2', '', title));
  if (description) node.append(element('p', 'muted', description));
  return node;
}

export function inputField(
  form: HTMLElement,
  name: string,
  label_text: string,
  options: {
    value?: string;
    type?: string;
    required?: boolean;
    hint?: string;
    placeholder?: string;
    multiline?: boolean;
  } = {},
): HTMLInputElement | HTMLTextAreaElement {
  const wrapper = element('div', 'form-field');
  const label = element('label', '', label_text);
  const input = options.multiline ? element('textarea') : element('input');
  input.id = name;
  input.name = name;
  input.value = options.value ?? '';
  input.required = options.required ?? false;
  input.placeholder = options.placeholder ?? '';
  if (input instanceof HTMLInputElement) input.type = options.type ?? 'text';
  if (input instanceof HTMLTextAreaElement) input.rows = 4;
  label.htmlFor = name;
  wrapper.append(label, input);
  if (options.hint) {
    const hint = element('p', 'field-hint', options.hint);
    hint.id = `${name}-hint`;
    input.setAttribute('aria-describedby', hint.id);
    wrapper.append(hint);
  }
  form.append(wrapper);
  return input;
}

export function statusMessage(): HTMLParagraphElement {
  const node = element('p', 'form-status');
  node.setAttribute('role', 'status');
  node.setAttribute('aria-live', 'polite');
  return node;
}

export function showError(node: HTMLElement, error: unknown): void {
  node.classList.add('error');
  node.textContent =
    error instanceof Error ? error.message : 'Something went wrong. Please try again.';
  if (error instanceof ApiError && error.status === 401) {
    node.append(document.createTextNode(' '), link('Sign in again →', '/account', 'text-link'));
  } else if (error instanceof ApiError && error.code === 'invalid_session_request') {
    node.append(
      document.createTextNode(' '),
      button('Refresh page', () => window.location.reload(), 'text-link nav-button'),
    );
  }
}

export function submitButton(text: string): HTMLButtonElement {
  const node = element('button', 'button primary', text);
  node.type = 'submit';
  return node;
}
