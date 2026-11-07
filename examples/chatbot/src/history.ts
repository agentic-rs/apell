export interface ConversationSummary {
  conversation_id: string;
  title: string;
  updated_at: string;
}

interface HistoryOptions {
  panel: HTMLElement;
  trigger: HTMLButtonElement;
  backdrop: HTMLButtonElement;
  onSelect: (id: string) => void;
  onCreate: () => void;
  onStop: () => void;
}

interface HistoryState {
  disabled: boolean;
  active: boolean;
  stopping: boolean;
}

export function createHistory({
  panel,
  trigger,
  backdrop,
  onSelect,
  onCreate,
  onStop,
}: HistoryOptions) {
  const root = panel.closest<HTMLElement>('#app') ?? document.documentElement;
  const mobile = window.matchMedia('(max-width: 760px)');
  const originalRole = panel.getAttribute('role');
  const backgroundInert = new Map<HTMLElement, boolean>();
  let conversations: readonly ConversationSummary[] = [];
  let currentId = '';
  let state: HistoryState = { disabled: true, active: false, stopping: false };
  let opened = false;
  let disposed = false;

  panel.id ||= 'companion-history';
  panel.tabIndex = -1;
  panel.setAttribute('aria-label', 'Conversations');
  panel.innerHTML = `
    <div class="history-header">
      <span class="history-brand">Companion</span>
      <button class="history-close" type="button" aria-label="Close conversations">×</button>
    </div>
    <button class="history-new" type="button"><span aria-hidden="true">＋</span> New chat</button>
    <label class="history-search">
      <span class="sr-only">Search conversations</span>
      <input type="search" placeholder="Search chats" autocomplete="off" />
    </label>
    <div class="history-status" role="status" hidden>
      <span></span><button class="history-stop" type="button">Stop reply</button>
    </div>
    <div class="history-section-title">Recent</div>
    <nav class="history-list" aria-label="Saved conversations"></nav>
    <p class="history-empty" hidden></p>
    <p class="history-footer">Saved on this device</p>`;

  const closeButton = panel.querySelector<HTMLButtonElement>('.history-close')!;
  const newButton = panel.querySelector<HTMLButtonElement>('.history-new')!;
  const search = panel.querySelector<HTMLInputElement>('input')!;
  const status = panel.querySelector<HTMLDivElement>('.history-status')!;
  const statusText = status.querySelector<HTMLSpanElement>('span')!;
  const stopButton = panel.querySelector<HTMLButtonElement>('.history-stop')!;
  const list = panel.querySelector<HTMLElement>('.history-list')!;
  const empty = panel.querySelector<HTMLParagraphElement>('.history-empty')!;

  trigger.setAttribute('aria-controls', panel.id);
  trigger.setAttribute('aria-label', 'Open conversations');
  backdrop.tabIndex = -1;
  backdrop.setAttribute('aria-hidden', 'true');

  function disableActions(): boolean {
    return state.disabled || state.active;
  }

  function renderList(resetScroll = false): void {
    const active = document.activeElement;
    const focusedId =
      active instanceof HTMLElement && list.contains(active)
        ? active.dataset.conversation_id
        : undefined;
    const scrollTop = resetScroll ? 0 : list.scrollTop;
    const query = search.value.trim().toLocaleLowerCase();
    const visible = conversations.filter((conversation) =>
      conversation.title.toLocaleLowerCase().includes(query),
    );
    const fragment = document.createDocumentFragment();
    let focusedRow: HTMLButtonElement | undefined;
    for (const conversation of visible) {
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'history-row';
      row.dataset.conversation_id = conversation.conversation_id;
      row.disabled = disableActions();
      if (conversation.conversation_id === currentId) {
        row.classList.add('is-current');
        row.setAttribute('aria-current', 'page');
      }
      const title = document.createElement('span');
      title.className = 'history-row-title';
      title.textContent = conversation.title || 'New conversation';
      row.append(title);
      fragment.append(row);
      if (conversation.conversation_id === focusedId) focusedRow = row;
    }
    list.replaceChildren(fragment);
    list.scrollTop = scrollTop;
    empty.hidden = visible.length > 0;
    empty.textContent = query ? 'No matching conversations.' : 'No conversations yet.';
    if (focusedId) {
      if (focusedRow && !focusedRow.disabled) focusedRow.focus({ preventScroll: true });
      else search.focus({ preventScroll: true });
    }
  }

  function setBackgroundInert(inert: boolean): void {
    if (inert) {
      for (const sibling of root.children) {
        if (
          !(sibling instanceof HTMLElement) ||
          sibling === panel ||
          sibling === backdrop ||
          sibling.contains(panel)
        )
          continue;
        if (!backgroundInert.has(sibling)) backgroundInert.set(sibling, sibling.inert);
        sibling.inert = true;
      }
    } else {
      for (const [element, previous] of backgroundInert) element.inert = previous;
      backgroundInert.clear();
    }
  }

  function applyMode(): void {
    const modal = mobile.matches && opened;
    const visible = !mobile.matches || opened;
    root.classList.toggle('history-open', modal);
    backdrop.hidden = !modal;
    panel.inert = !visible;
    panel.setAttribute('aria-hidden', String(!visible));
    trigger.setAttribute('aria-expanded', String(visible));
    setBackgroundInert(modal);
    if (modal) {
      panel.setAttribute('role', 'dialog');
      panel.setAttribute('aria-modal', 'true');
    } else {
      panel.removeAttribute('aria-modal');
      if (originalRole) panel.setAttribute('role', originalRole);
      else panel.removeAttribute('role');
    }
  }

  function close(restoreFocus = true): void {
    const restore = opened && mobile.matches && restoreFocus;
    opened = false;
    applyMode();
    if (restore && trigger.isConnected) trigger.focus({ preventScroll: true });
  }

  function open(): void {
    if (disposed || !mobile.matches) return;
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    opened = true;
    applyMode();
    closeButton.focus({ preventScroll: true });
  }

  function toggle(): void {
    if (opened) close();
    else open();
  }

  function handleModeChange(): void {
    const hiddenFocus = mobile.matches && panel.contains(document.activeElement);
    opened = false;
    applyMode();
    if (hiddenFocus) trigger.focus({ preventScroll: true });
  }

  function handleKey(event: KeyboardEvent): void {
    if (!opened || !mobile.matches) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      close();
      return;
    }
    if (event.key !== 'Tab') return;
    const focusable = [
      ...panel.querySelectorAll<HTMLElement>(
        'button:not(:disabled), input:not(:disabled), [tabindex="0"]',
      ),
    ].filter((element) => element.getClientRects().length > 0);
    const first = focusable[0];
    const last = focusable.at(-1);
    const active = document.activeElement;
    if (!first) {
      event.preventDefault();
      panel.focus({ preventScroll: true });
    } else if (!panel.contains(active) || active === panel) {
      event.preventDefault();
      (event.shiftKey ? last! : first).focus({ preventScroll: true });
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last!.focus({ preventScroll: true });
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus({ preventScroll: true });
    }
  }

  function selectConversation(event: MouseEvent): void {
    const row =
      event.target instanceof Element
        ? event.target.closest<HTMLButtonElement>('.history-row')
        : null;
    const id = row?.dataset.conversation_id;
    if (!id || !row || !list.contains(row) || disableActions()) return;
    close();
    onSelect(id);
  }

  function createConversation(): void {
    if (disableActions()) return;
    close();
    onCreate();
  }

  function stopReply(): void {
    if (state.active && !state.stopping) onStop();
  }

  function filterConversations(): void {
    renderList(true);
  }

  const dismiss = () => close();
  trigger.addEventListener('click', toggle);
  closeButton.addEventListener('click', dismiss);
  backdrop.addEventListener('click', dismiss);
  newButton.addEventListener('click', createConversation);
  stopButton.addEventListener('click', stopReply);
  list.addEventListener('click', selectConversation);
  search.addEventListener('input', filterConversations);
  document.addEventListener('keydown', handleKey);
  mobile.addEventListener('change', handleModeChange);
  applyMode();

  return {
    update(
      next: readonly ConversationSummary[],
      current_id: string,
      nextState: HistoryState,
    ): void {
      if (disposed) return;
      const changed =
        conversations !== next ||
        currentId !== current_id ||
        state.disabled !== nextState.disabled ||
        state.active !== nextState.active;
      conversations = next;
      currentId = current_id;
      state = nextState;
      newButton.disabled = disableActions();
      status.hidden = !state.active;
      statusText.textContent = state.stopping
        ? 'Stopping reply…'
        : 'Reply in progress. Stop it to switch chats.';
      stopButton.disabled = state.stopping;
      stopButton.textContent = state.stopping ? 'Stopping…' : 'Stop reply';
      if (changed) renderList();
    },
    close,
    dispose(): void {
      if (disposed) return;
      disposed = true;
      close(false);
      setBackgroundInert(false);
      panel.inert = false;
      panel.removeAttribute('aria-hidden');
      trigger.removeEventListener('click', toggle);
      closeButton.removeEventListener('click', dismiss);
      backdrop.removeEventListener('click', dismiss);
      newButton.removeEventListener('click', createConversation);
      stopButton.removeEventListener('click', stopReply);
      list.removeEventListener('click', selectConversation);
      search.removeEventListener('input', filterConversations);
      document.removeEventListener('keydown', handleKey);
      mobile.removeEventListener('change', handleModeChange);
    },
  };
}
