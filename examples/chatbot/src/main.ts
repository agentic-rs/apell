import {
  applet,
  llm,
  type LlmConfiguration,
  type LlmTarget,
} from '../../../packages/applet-sdk/src';
import { createComposerMedia, type ComposerDraft } from './composer';
import { createHistory, type ConversationSummary } from './history';
import { createMediaRenderer, pauseMediaPlayback, type MediaAsset } from './media';
import './style.css';

interface Message {
  message_id: string;
  role: 'user' | 'assistant';
  content: string;
  status: 'complete' | 'pending' | 'interrupted';
  media: MediaAsset[];
}

const root =
  document.getElementById('app') ?? document.body.appendChild(document.createElement('div'));
root.id = 'app';
root.innerHTML = `
  <button class="history-backdrop" type="button" aria-label="Close conversations" tabindex="-1" hidden></button>
  <aside class="sidebar" id="conversations-panel" aria-label="Conversations"></aside>
  <main class="chat">
    <header class="chat-header">
      <button class="header-icon history-toggle" type="button" aria-label="Open conversations" aria-controls="conversations-panel" aria-expanded="false">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M4 6h16M4 12h11M4 18h7" /></svg>
      </button>
      <div class="chat-heading"><h1>New chat</h1>
      <button class="model-pill" type="button" disabled>Choose model ▾</button>
      </div>
      <button class="header-icon header-new" type="button" aria-label="New chat" title="New chat">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M13 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8M16 3l5 5M10 14l1-5 8-8 5 5-8 8-6 1Z" transform="translate(0 1) scale(.9)" /></svg>
      </button>
    </header>
    <div class="error-banner" role="alert" hidden></div>
    <div class="messages" aria-live="polite" aria-relevant="additions text">
      <div class="loading">Opening your conversations…</div>
    </div>
    <form class="composer">
      <label class="sr-only" for="message">Message</label>
      <textarea id="message" rows="1" maxlength="32768" enterkeyhint="send" placeholder="Message Companion" disabled></textarea>
      <div class="composer-bottom">
        <button class="send composer-action" type="submit" aria-label="Send message" title="Send message" disabled><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 19V5m-6 6 6-6 6 6" /></svg></button>
        <button class="stop composer-action" type="button" aria-label="Stop reply" title="Stop reply" hidden><svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="2" /></svg></button>
      </div>
    </form>
    <p class="footer-note"></p>
  </main>`;

function element<T extends HTMLElement>(selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`Missing chatbot element: ${selector}`);
  return value;
}

const messagesElement = element<HTMLDivElement>('.messages');
const errorElement = element<HTMLDivElement>('.error-banner');
const textarea = element<HTMLTextAreaElement>('textarea');
const sendButton = element<HTMLButtonElement>('.send');
const stopButton = element<HTMLButtonElement>('.stop');
const modelButton = element<HTMLButtonElement>('.model-pill');
const footer = element<HTMLParagraphElement>('.footer-note');
let conversations: ConversationSummary[] = [];
let currentId = '';
let selected: LlmConfiguration | null = null;
let changingView = false;
interface Reply {
  request_id: string;
  conversation_id: string;
  assistant_id: string;
  partial: string;
  stopping: boolean;
}
interface Snapshot {
  revision: number;
  conversation_id: string;
  conversations: ConversationSummary[];
  messages: Message[];
  target: LlmTarget | null;
  active: Reply | null;
  error: string | null;
}
let activeRequest: Reply | null = null;
let streamNode: HTMLParagraphElement | undefined;
let revision = -1;
let ready = false;
const messageMedia = createMediaRenderer(showError);
const composer = createComposerMedia(element<HTMLFormElement>('form'), updateControls, showError);
const history = createHistory({
  panel: element<HTMLElement>('.sidebar'),
  trigger: element<HTMLButtonElement>('.history-toggle'),
  backdrop: element<HTMLButtonElement>('.history-backdrop'),
  onSelect: (conversation_id) => {
    void changeView(() => applet.invoke('open_conversation', { conversation_id }));
  },
  onCreate: newConversation,
  onStop: stopReply,
});
const drafts = new Map<string, { text: string; composer: ComposerDraft }>();
const scrollPositions = new Map<string, number>();
const renderedMessages: {
  message_id: string;
  media_key: string;
  article: HTMLElement;
  text: HTMLParagraphElement;
}[] = [];
window.addEventListener(
  'pagehide',
  () => {
    messageMedia.dispose();
    composer.dispose();
    history.dispose();
  },
  { once: true },
);

function showError(error?: unknown): void {
  errorElement.hidden = error === undefined;
  errorElement.textContent =
    error instanceof Error ? error.message : error === undefined ? '' : String(error);
}

function scrollToBottom(): void {
  messagesElement.scrollTop = messagesElement.scrollHeight;
}

function updateControls(): void {
  const busy = !ready || !!activeRequest || changingView;
  composer.update(busy, selected);
  const submitting = busy || composer.isBusy();
  textarea.disabled = submitting;
  sendButton.hidden = !!activeRequest;
  sendButton.disabled =
    submitting ||
    !selected ||
    !composer.valid() ||
    (!textarea.value.trim() && !composer.attachments().length);
  stopButton.hidden = !activeRequest;
  stopButton.disabled = !!activeRequest?.stopping;
  modelButton.disabled = submitting;
  element<HTMLButtonElement>('.header-new').disabled = submitting;
  element<HTMLButtonElement>('.history-toggle').disabled = !ready || changingView;
  history.update(conversations, currentId, {
    disabled: submitting,
    active: !!activeRequest,
    stopping: !!activeRequest?.stopping,
  });
}

function renderHistory(): void {
  element<HTMLHeadingElement>('h1').textContent =
    conversations
      .find((value) => value.conversation_id === currentId)
      ?.title.replace(/^New conversation$/, 'New chat') ?? 'New chat';
  updateControls();
}

function appendMessage(message: Message): HTMLParagraphElement {
  const article = document.createElement('article');
  article.className = `message ${message.role}`;
  const label = document.createElement('div');
  label.className = 'message-label';
  label.textContent = message.role === 'user' ? 'You' : 'Companion';
  const text = document.createElement('p');
  text.className = 'message-content';
  article.append(label, text);
  messageMedia.render(article, message.media ?? []);
  updateMessage(article, text, message);
  messagesElement.appendChild(article);
  renderedMessages.push({
    message_id: message.message_id,
    media_key: JSON.stringify(message.media ?? []),
    article,
    text,
  });
  return text;
}

function updateMessage(article: HTMLElement, text: HTMLParagraphElement, message: Message): void {
  text.textContent =
    message.content ||
    (message.status === 'pending'
      ? 'Thinking…'
      : message.media?.length
        ? ''
        : 'No reply received.');
  text.hidden = !text.textContent;
  article.querySelector('.message-status')?.remove();
  if (message.status === 'interrupted') {
    const status = document.createElement('span');
    status.className = 'message-status';
    status.textContent = 'Reply interrupted';
    article.appendChild(status);
  }
}

function renderMessages(messages: Message[], switched: boolean): void {
  const scrollTop = messagesElement.scrollTop;
  const savedPosition = switched ? scrollPositions.get(currentId) : undefined;
  const follow = switched
    ? savedPosition === undefined
    : messagesElement.scrollHeight - scrollTop - messagesElement.clientHeight < 100;
  const rebuild =
    switched ||
    !renderedMessages.length ||
    renderedMessages.some((rendered, index) => {
      const message = messages[index];
      return (
        !message ||
        message.message_id !== rendered.message_id ||
        JSON.stringify(message.media ?? []) !== rendered.media_key
      );
    });
  if (rebuild) {
    messageMedia.reset();
    renderedMessages.length = 0;
    messagesElement.replaceChildren();
  }
  streamNode = undefined;
  if (messages.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.innerHTML = `<h2>How can I help?</h2><p>A question, an idea, a fresh perspective.</p><div class="suggestions"><button type="button">Brainstorm an idea</button><button type="button">Explain something</button><button type="button">Make a plan</button></div>`;
    for (const button of empty.querySelectorAll<HTMLButtonElement>('button')) {
      button.addEventListener('click', () => {
        textarea.value = button.textContent ?? '';
        resizeInput();
        updateControls();
        textarea.focus({ preventScroll: true });
      });
    }
    messagesElement.appendChild(empty);
    messagesElement.scrollTop = 0;
  } else {
    for (const [index, message] of messages.entries()) {
      const rendered = renderedMessages[index];
      const node = rendered?.text ?? appendMessage(message);
      if (rendered) updateMessage(rendered.article, node, message);
      if (activeRequest?.assistant_id === message.message_id) {
        streamNode = node;
        node.textContent = activeRequest.partial || 'Thinking…';
        node.hidden = false;
      }
    }
    if (follow) scrollToBottom();
    else messagesElement.scrollTop = savedPosition ?? scrollTop;
  }
}

function targetOf({ connection_id, model_id }: LlmTarget) {
  return { connection_id, model_id };
}

async function applySnapshot(view: Snapshot): Promise<void> {
  let model: LlmConfiguration | null = null;
  let modelError: unknown;
  if (view.target) {
    try {
      const config = await llm.config(view.target);
      if (config.mode === 'provider') model = config;
    } catch (error) {
      modelError = error;
    }
  }
  if (view.revision < revision) return;
  const switched = view.conversation_id !== currentId;
  if (switched) await pauseMediaPlayback();
  if (view.revision < revision) return;
  revision = view.revision;
  if (switched && currentId) {
    drafts.set(currentId, { text: textarea.value, composer: composer.draft() });
    scrollPositions.set(currentId, messagesElement.scrollTop);
  }
  currentId = view.conversation_id;
  conversations = view.conversations;
  activeRequest = view.active;
  selected = model;
  if (switched) {
    const draft = drafts.get(currentId);
    textarea.value = draft?.text ?? '';
    composer.update(true, model);
    composer.restoreDraft(draft?.composer);
    resizeInput();
  }
  const label = model ? `${model.provider_name} · ${model.model_id}` : 'Choose model';
  modelButton.textContent = `${label} ▾`;
  modelButton.title = model
    ? `Change provider and model (${label})`
    : 'Choose a provider and model';
  modelButton.setAttribute('aria-label', modelButton.title);
  footer.textContent = view.messages.length
    ? 'Changing models starts a new chat.'
    : model
      ? ''
      : 'Choose a model to start chatting.';
  footer.hidden = !footer.textContent;
  showError(modelError ?? view.error ?? undefined);
  renderHistory();
  renderMessages(view.messages, switched);
}

// Subscribe before the first snapshot, coalescing refreshes during a slow host
// call. A missed delta or a newly attached view always recovers the full state.
let queued = false;
let refreshing: Promise<void> | undefined;
function refresh(): Promise<void> {
  queued = true;
  refreshing ??= (async () => {
    try {
      while (queued) {
        queued = false;
        await applySnapshot(await applet.invoke<Snapshot>('snapshot'));
      }
    } finally {
      refreshing = undefined;
    }
  })();
  return refreshing;
}

async function changeView(action: () => Promise<unknown>): Promise<void> {
  if (!ready || activeRequest || changingView || composer.isBusy()) return;
  changingView = true;
  updateControls();
  showError();
  try {
    await action();
    await refresh();
  } catch (error) {
    showError(error);
  } finally {
    changingView = false;
    updateControls();
  }
}

function send(): void {
  const content = textarea.value.trim();
  const media = composer.attachments();
  if (
    !ready ||
    activeRequest ||
    changingView ||
    composer.isBusy() ||
    !selected ||
    !composer.valid() ||
    (!content && !media.length)
  )
    return;
  void changeView(async () => {
    const output = composer.output();
    await applet.invoke('start_send', {
      conversation_id: currentId,
      content,
      media,
      ...(output ? { output } : {}),
    });
    textarea.value = '';
    composer.clearSent();
    drafts.delete(currentId);
    resizeInput();
  });
}

function newConversation(): void {
  void changeView(() =>
    applet.invoke('create_conversation', { target: selected ? targetOf(selected) : null }),
  );
}

element<HTMLFormElement>('form').addEventListener('submit', (event) => {
  event.preventDefault();
  send();
});
function resizeInput(): void {
  textarea.style.height = 'auto';
  textarea.style.height = `${Math.min(textarea.scrollHeight, 160)}px`;
}
textarea.addEventListener('input', () => {
  resizeInput();
  updateControls();
});
textarea.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    send();
  }
});
function stopReply(): void {
  if (!activeRequest) return;
  stopButton.disabled = true;
  void applet
    .invoke('stop', { request_id: activeRequest.request_id })
    .then(refresh)
    .catch(showError);
}
stopButton.addEventListener('click', stopReply);
element<HTMLButtonElement>('.header-new').addEventListener('click', newConversation);
modelButton.addEventListener('click', () => {
  void changeView(async () => {
    const result = await llm.chooseModel(selected ? { current: targetOf(selected) } : {});
    if (result)
      await applet.invoke('choose_model', { conversation_id: currentId, target: targetOf(result) });
  });
});

async function boot(): Promise<void> {
  applet.onEvent((event) => {
    if (event.name !== 'chat_changed') return;
    const change = event.payload as {
      kind: string;
      revision: number;
      request_id?: string;
      text?: string;
    };
    if (change.revision <= revision) return;
    if (
      change.kind === 'delta' &&
      change.revision === revision + 1 &&
      change.request_id === activeRequest?.request_id &&
      activeRequest &&
      streamNode &&
      typeof change.text === 'string'
    ) {
      revision = change.revision;
      const follow =
        messagesElement.scrollHeight - messagesElement.scrollTop - messagesElement.clientHeight <
        100;
      activeRequest.partial += change.text;
      streamNode.textContent = activeRequest.partial;
      if (follow) scrollToBottom();
    } else {
      void refresh().catch(showError);
    }
  });
  await refresh();
  ready = true;
  updateControls();
}

void boot().catch((error) => {
  showError(error);
  messagesElement.textContent =
    'The conversation could not be loaded. Reload this applet to try again.';
});
