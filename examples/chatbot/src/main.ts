import { applet, llm, type LlmSelection, type LlmTarget } from '../../../packages/applet-sdk/src';
import './style.css';

interface Conversation {
  conversation_id: string;
  title: string;
  updated_at: string;
}

interface Message {
  message_id: string;
  role: 'user' | 'assistant';
  content: string;
  status: 'complete' | 'pending' | 'interrupted';
}

const root =
  document.getElementById('app') ?? document.body.appendChild(document.createElement('div'));
root.id = 'app';
root.innerHTML = `
  <aside class="sidebar">
    <div class="brand"><span class="brand-mark" aria-hidden="true">✳</span><div>Companion<span>A little room to think</span></div></div>
    <button class="new-chat" type="button"><span aria-hidden="true">＋</span> New conversation</button>
    <div class="section-label">YOUR CONVERSATIONS</div>
    <nav class="conversation-list" aria-label="Conversations"></nav>
    <div class="sidebar-footer"><span class="local-dot"></span> Saved on this device</div>
  </aside>
  <main class="chat">
    <header class="chat-header">
      <div><p class="eyebrow">COMPANION</p><h1>New conversation</h1></div>
      <button class="model-pill" type="button" disabled>Choose model ▾</button>
    </header>
    <div class="mobile-history"><select aria-label="Conversation"></select><button type="button" class="mobile-new" aria-label="New conversation">＋</button></div>
    <div class="error-banner" role="alert" hidden></div>
    <div class="messages" aria-live="polite" aria-relevant="additions text">
      <div class="loading">Opening your conversations…</div>
    </div>
    <form class="composer">
      <label class="sr-only" for="message">Message</label>
      <textarea id="message" rows="2" maxlength="32768" enterkeyhint="send" placeholder="Ask a question, explore an idea…" disabled></textarea>
      <div class="composer-bottom"><span class="composer-hint">Enter to send · Shift + Enter for a new line</span><button class="send" type="submit" disabled>Send <span aria-hidden="true">↑</span></button><button class="stop" type="button" hidden>Stop reply <span aria-hidden="true">■</span></button></div>
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
const select = element<HTMLSelectElement>('select');
const modelButton = element<HTMLButtonElement>('.model-pill');
const footer = element<HTMLParagraphElement>('.footer-note');
let conversations: Conversation[] = [];
let currentId = '';
let selected: LlmSelection | null = null;
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
  conversations: Conversation[];
  messages: Message[];
  target: LlmTarget | null;
  active: Reply | null;
  error: string | null;
}
let activeRequest: Reply | null = null;
let streamNode: HTMLParagraphElement | undefined;
let revision = -1;
let ready = false;

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
  textarea.disabled = busy;
  sendButton.hidden = !!activeRequest;
  sendButton.disabled = busy || !selected || !textarea.value.trim();
  stopButton.hidden = !activeRequest;
  stopButton.disabled = !!activeRequest?.stopping;
  modelButton.disabled = busy;
  select.disabled = busy;
  for (const button of root.querySelectorAll<HTMLButtonElement>(
    '.conversation-list button, .new-chat, .mobile-new',
  )) {
    button.disabled = busy;
  }
}

function renderHistory(): void {
  const list = element<HTMLElement>('.conversation-list');
  list.replaceChildren();
  select.replaceChildren();
  for (const conversation of conversations) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className =
      conversation.conversation_id === currentId ? 'conversation active' : 'conversation';
    button.textContent = conversation.title;
    button.title = conversation.title;
    button.setAttribute(
      'aria-current',
      conversation.conversation_id === currentId ? 'page' : 'false',
    );
    button.addEventListener('click', () => {
      void changeView(() =>
        applet.invoke('open_conversation', { conversation_id: conversation.conversation_id }),
      );
    });
    list.appendChild(button);
    const option = document.createElement('option');
    option.value = conversation.conversation_id;
    option.textContent = conversation.title;
    option.selected = conversation.conversation_id === currentId;
    select.appendChild(option);
  }
  element<HTMLHeadingElement>('h1').textContent =
    conversations.find((value) => value.conversation_id === currentId)?.title ?? 'New conversation';
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
  text.textContent =
    message.content || (message.status === 'pending' ? 'Thinking…' : 'No reply received.');
  article.append(label, text);
  if (message.status === 'interrupted') {
    const status = document.createElement('span');
    status.className = 'message-status';
    status.textContent = 'Reply interrupted';
    article.appendChild(status);
  }
  messagesElement.appendChild(article);
  return text;
}

function renderMessages(messages: Message[]): void {
  messagesElement.replaceChildren();
  if (messages.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.innerHTML = `<div class="empty-symbol" aria-hidden="true">✳</div><h2>What’s on your mind?</h2><p>Ask a question or explore an idea.</p><div class="suggestions"><button type="button">Help me think through an idea</button><button type="button">Explain something simply</button><button type="button">Make a plan for today</button></div>`;
    for (const button of empty.querySelectorAll<HTMLButtonElement>('button')) {
      button.addEventListener('click', () => {
        textarea.value = button.textContent ?? '';
        updateControls();
        textarea.focus({ preventScroll: true });
      });
    }
    messagesElement.appendChild(empty);
    messagesElement.scrollTop = 0;
  } else {
    streamNode = undefined;
    for (const message of messages) {
      const node = appendMessage(message);
      if (activeRequest?.assistant_id === message.message_id) {
        streamNode = node;
        node.textContent = activeRequest.partial || 'Thinking…';
      }
    }
    scrollToBottom();
  }
}

function targetOf({ connection_id, model_id }: LlmTarget) {
  return { connection_id, model_id };
}

async function applySnapshot(view: Snapshot): Promise<void> {
  let model: LlmSelection | null = null;
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
  revision = view.revision;
  currentId = view.conversation_id;
  conversations = view.conversations;
  activeRequest = view.active;
  selected = model;
  const label = model ? `${model.provider_name} · ${model.model_id}` : 'Choose model';
  modelButton.textContent = `${label} ▾`;
  modelButton.title = model
    ? `Change provider and model (${label})`
    : 'Choose a provider and model';
  modelButton.setAttribute('aria-label', modelButton.title);
  footer.textContent = view.messages.length
    ? 'Choosing a different provider or model starts a new conversation.'
    : model
      ? 'Messages use the provider and model shown above.'
      : 'Choose a provider and model to send a message.';
  showError(modelError ?? view.error ?? undefined);
  renderHistory();
  renderMessages(view.messages);
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
  if (!ready || activeRequest || changingView) return;
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
  if (!ready || activeRequest || changingView || !selected || !content) return;
  void changeView(async () => {
    await applet.invoke('start_send', { conversation_id: currentId, content });
    textarea.value = '';
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
textarea.addEventListener('input', updateControls);
textarea.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    send();
  }
});
stopButton.addEventListener('click', () => {
  if (!activeRequest) return;
  stopButton.disabled = true;
  void applet
    .invoke('stop', { request_id: activeRequest.request_id })
    .then(refresh)
    .catch(showError);
});
element<HTMLButtonElement>('.new-chat').addEventListener('click', newConversation);
element<HTMLButtonElement>('.mobile-new').addEventListener('click', newConversation);
select.addEventListener('change', () => {
  void changeView(() => applet.invoke('open_conversation', { conversation_id: select.value }));
});
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
