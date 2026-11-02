import init, {
  initialize,
  list_conversations,
  create_conversation,
  conversation_model,
  choose_model,
  load_messages,
  send_message,
} from '../pkg/chatbot.js';
import {
  createId,
  HostError,
  hostCall,
  llm,
  type LlmRequest,
  type LlmSelection,
  type LlmTarget,
} from '../../../packages/applet-sdk/src';
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
let activeRequest: { request_id: string; controller: AbortController } | undefined;
let streamNode: HTMLParagraphElement | undefined;
let streamText = '';
let ready = false;

Object.assign(globalThis, {
  apellCreateId: createId,
  apellHostCall: (method: string, params: Record<string, unknown>) => {
    if (method !== 'llm.generate') return hostCall(method, params);
    const active = activeRequest;
    if (!active || params.request_id !== active.request_id) {
      return Promise.reject(new HostError('This reply is no longer active.'));
    }
    return llm.generate(params as unknown as LlmRequest, {
      signal: active.controller.signal,
      on_event: (event) => {
        if (activeRequest !== active || event.kind !== 'text_delta' || !streamNode) return;
        const follow =
          messagesElement.scrollHeight - messagesElement.scrollTop - messagesElement.clientHeight <
          100;
        streamText += event.text;
        streamNode.textContent = streamText;
        if (follow) scrollToBottom();
      },
    });
  },
});

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
  stopButton.disabled = !!activeRequest?.controller.signal.aborted;
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
      void changeView(() => openConversation(conversation.conversation_id));
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
    messages.forEach(appendMessage);
    scrollToBottom();
  }
}

async function refreshHistory(): Promise<void> {
  conversations = (await list_conversations()) as Conversation[];
  renderHistory();
}

function targetOf({ connection_id, model_id }: LlmTarget): LlmTarget {
  return { connection_id, model_id };
}

async function changeView(operation: () => Promise<void>): Promise<void> {
  if (!ready || activeRequest || changingView) return;
  changingView = true;
  updateControls();
  showError();
  try {
    await operation();
  } catch (error) {
    showError(error);
  } finally {
    changingView = false;
    updateControls();
  }
}

async function openConversation(conversationId: string): Promise<void> {
  const messages = (await load_messages(conversationId)) as Message[];
  const target = (await conversation_model(conversationId)) as LlmTarget | null;
  selected = null;
  try {
    // Only an empty chat may adopt the shell default. Populated legacy chats
    // without a known route remain readable until the user starts a fresh one.
    if (target || messages.length === 0) {
      const config = await llm.config(target ?? undefined);
      if (config.mode === 'provider') {
        if (!target) await choose_model(conversationId, targetOf(config));
        selected = config;
      }
    }
  } catch (error) {
    showError(error);
  }
  currentId = conversationId;
  const label = selected ? `${selected.provider_name} · ${selected.model_id}` : 'Choose model';
  modelButton.textContent = `${label} ▾`;
  modelButton.title = selected
    ? `Change provider and model (${label})`
    : 'Choose a provider and model';
  modelButton.setAttribute('aria-label', modelButton.title);
  footer.textContent = messages.length
    ? 'Choosing a different provider or model starts a new conversation.'
    : selected
      ? 'Messages use the provider and model shown above.'
      : 'Choose a provider and model to send a message.';
  renderHistory();
  renderMessages(messages);
}

async function newConversation(): Promise<void> {
  const id = await create_conversation();
  if (selected) await choose_model(id, targetOf(selected));
  await refreshHistory();
  await openConversation(id);
}

async function send(): Promise<void> {
  const content = textarea.value.trim();
  if (!ready || activeRequest || changingView || !selected || !content) return;
  const active = { request_id: createId(), controller: new AbortController() };
  activeRequest = active;
  streamText = '';
  showError();
  updateControls();
  if (messagesElement.querySelector('.empty-state')) messagesElement.replaceChildren();
  appendMessage({ message_id: '', role: 'user', content, status: 'complete' });
  streamNode = appendMessage({ message_id: '', role: 'assistant', content: '', status: 'pending' });
  textarea.value = '';
  scrollToBottom();
  try {
    await send_message(currentId, content, active.request_id);
  } catch (error) {
    if (!active.controller.signal.aborted) showError(error);
  } finally {
    streamNode = undefined;
    try {
      await refreshHistory();
      await openConversation(currentId);
    } catch (error) {
      showError(error);
    }
    activeRequest = undefined;
    updateControls();
    textarea.focus({ preventScroll: true });
  }
}

element<HTMLFormElement>('form').addEventListener('submit', (event) => {
  event.preventDefault();
  void send();
});
textarea.addEventListener('input', updateControls);
textarea.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    void send();
  }
});
stopButton.addEventListener('click', () => {
  if (!activeRequest) return;
  stopButton.disabled = true;
  activeRequest.controller.abort();
});
element<HTMLButtonElement>('.new-chat').addEventListener('click', () => {
  void changeView(newConversation);
});
element<HTMLButtonElement>('.mobile-new').addEventListener('click', () => {
  void changeView(newConversation);
});
select.addEventListener('change', () => {
  void changeView(() => openConversation(select.value));
});
modelButton.addEventListener('click', () => {
  void changeView(async () => {
    const result = await llm.chooseModel(selected ? { current: targetOf(selected) } : {});
    if (!result) return;
    const id = await choose_model(currentId, targetOf(result));
    await refreshHistory();
    await openConversation(id);
  });
});

async function boot(): Promise<void> {
  await init({
    module_or_path: Uint8Array.from(atob('__WASM_BASE64__'), (character) =>
      character.charCodeAt(0),
    ),
  });
  await initialize();
  await refreshHistory();
  currentId = conversations[0]?.conversation_id ?? (await create_conversation());
  await refreshHistory();
  await openConversation(currentId);
  ready = true;
  updateControls();
}

void boot().catch((error) => {
  showError(error);
  messagesElement.replaceChildren();
  const retry = document.createElement('p');
  retry.className = 'loading';
  retry.textContent = 'The chatbot could not start. Close it and open it again from your library.';
  messagesElement.appendChild(retry);
});
