import init, {
  initialize,
  list_conversations,
  create_conversation,
  load_messages,
  send_message,
} from '../pkg/chatbot.js';
import { createId, HostError, hostCall, llm, onLlmEvent } from '../../../packages/applet-sdk/src';
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

const canceled = new Set<string>();
Object.assign(globalThis, {
  apellCreateId: createId,
  apellHostCall: (method: string, params: Record<string, unknown>) => {
    if (
      (method === 'llm.complete' || method === 'llm.generate') &&
      typeof params.request_id === 'string' &&
      canceled.has(params.request_id)
    ) {
      return Promise.reject(new HostError('Reply stopped.'));
    }
    return hostCall(method, params);
  },
});

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
      <span class="model-pill">Connecting…</span>
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
    <p class="footer-note">Your conversations stay here. Requests use your shell’s shared model.</p>
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
let conversations: Conversation[] = [];
let currentId = '';
let activeRequest: string | undefined;
let streamNode: HTMLParagraphElement | undefined;
let streamText = '';
let ready = false;

function showError(error?: unknown): void {
  errorElement.hidden = error === undefined;
  errorElement.textContent =
    error instanceof Error ? error.message : error === undefined ? '' : String(error);
}

function scrollToBottom(): void {
  messagesElement.scrollTop = messagesElement.scrollHeight;
}

function setBusy(busy: boolean): void {
  textarea.disabled = !ready || busy;
  sendButton.hidden = busy;
  sendButton.disabled = !ready || !textarea.value.trim();
  stopButton.hidden = !busy;
  stopButton.disabled = false;
  select.disabled = busy;
  for (const button of root.querySelectorAll<HTMLButtonElement>(
    '.conversation-list button, .new-chat, .mobile-new',
  )) {
    button.disabled = !ready || busy;
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
      void openConversation(conversation.conversation_id).catch(showError);
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
  setBusy(Boolean(activeRequest));
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
        setBusy(false);
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

async function openConversation(conversationId: string): Promise<void> {
  if (activeRequest) return;
  showError();
  const messages = (await load_messages(conversationId)) as Message[];
  currentId = conversationId;
  renderHistory();
  renderMessages(messages);
}

async function newConversation(): Promise<void> {
  if (!ready || activeRequest) return;
  currentId = await create_conversation();
  await refreshHistory();
  renderMessages([]);
  showError();
  textarea.focus({ preventScroll: true });
}

async function send(): Promise<void> {
  const content = textarea.value.trim();
  if (!ready || activeRequest || !content) return;
  const request_id = createId();
  activeRequest = request_id;
  streamText = '';
  showError();
  setBusy(true);
  if (messagesElement.querySelector('.empty-state')) messagesElement.replaceChildren();
  appendMessage({ message_id: '', role: 'user', content, status: 'complete' });
  streamNode = appendMessage({ message_id: '', role: 'assistant', content: '', status: 'pending' });
  textarea.value = '';
  scrollToBottom();
  try {
    await send_message(currentId, content, request_id);
  } catch (error) {
    if (!canceled.has(request_id)) showError(error);
  } finally {
    activeRequest = undefined;
    streamNode = undefined;
    canceled.delete(request_id);
    try {
      await refreshHistory();
      renderMessages((await load_messages(currentId)) as Message[]);
    } catch (error) {
      showError(error);
    }
    setBusy(false);
    textarea.focus({ preventScroll: true });
  }
}

onLlmEvent((event) => {
  if (event.request_id !== activeRequest || event.kind !== 'delta' || !streamNode) return;
  const follow =
    messagesElement.scrollHeight - messagesElement.scrollTop - messagesElement.clientHeight < 100;
  streamText += event.text;
  streamNode.textContent = streamText;
  if (follow) scrollToBottom();
});

element<HTMLFormElement>('form').addEventListener('submit', (event) => {
  event.preventDefault();
  void send();
});
textarea.addEventListener('input', () => {
  sendButton.disabled = !textarea.value.trim();
});
textarea.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    void send();
  }
});
stopButton.addEventListener('click', () => {
  if (!activeRequest) return;
  canceled.add(activeRequest);
  stopButton.disabled = true;
  void llm.cancel(activeRequest).catch((error) => {
    showError(error);
    stopButton.disabled = false;
  });
});
element<HTMLButtonElement>('.new-chat').addEventListener('click', () => {
  void newConversation().catch(showError);
});
element<HTMLButtonElement>('.mobile-new').addEventListener('click', () => {
  void newConversation().catch(showError);
});
select.addEventListener('change', () => {
  void openConversation(select.value).catch(showError);
});

async function boot(): Promise<void> {
  await init({
    module_or_path: Uint8Array.from(atob('__WASM_BASE64__'), (character) =>
      character.charCodeAt(0),
    ),
  });
  await initialize();
  const configuration = await llm.config();
  element<HTMLSpanElement>('.model-pill').textContent =
    configuration.mode === 'demo' ? 'Demo mode' : configuration.model_id;
  if (configuration.mode === 'demo') {
    element<HTMLParagraphElement>('.footer-note').textContent =
      'Demo replies are generated locally. Configure a provider in the shell for real AI responses.';
  }
  ready = true;
  await refreshHistory();
  currentId = conversations[0]?.conversation_id ?? (await create_conversation());
  await refreshHistory();
  await openConversation(currentId);
}

void boot().catch((error) => {
  showError(error);
  messagesElement.replaceChildren();
  const retry = document.createElement('p');
  retry.className = 'loading';
  retry.textContent = 'The chatbot could not start. Close it and open it again from your library.';
  messagesElement.appendChild(retry);
});
