import {
  hostCall,
  photos,
  fs,
  type LlmConfiguration,
  type LlmMediaOutput,
} from '../../../packages/applet-sdk/src';
import { createMediaRenderer, validateInputMedia, type MediaAsset } from './media';

const MAX_MEDIA_BYTES = 8 * 1024 * 1024;
const MAX_MEDIA_FILES = 8;

export interface ComposerDraft {
  media: MediaAsset[];
  reply_mode: 'text' | 'image' | 'audio';
  voice: string;
}

export function createComposerMedia(
  form: HTMLFormElement,
  onChange: () => void,
  onError: (error?: unknown) => void,
) {
  const controls = document.createElement('div');
  controls.className = 'composer-media';
  controls.innerHTML = `
    <div class="draft-media" aria-label="Attachments" hidden></div>
    <label class="voice-option" hidden>Voice
      <input aria-label="Audio voice" value="alloy" maxlength="128" autocomplete="off" autocapitalize="none" spellcheck="false" />
    </label>
    <p class="media-notice" role="status" hidden></p>`;
  form.prepend(controls);
  const tools = document.createElement('div');
  tools.className = 'composer-tools';
  tools.innerHTML = `
    <div class="attachment-control">
      <button type="button" class="attachment-toggle" aria-label="Add attachment" title="Add attachment" aria-expanded="false" aria-controls="companion-attachment-menu">
        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
      </button>
      <div class="attachment-menu" id="companion-attachment-menu" role="group" aria-label="Add attachment" hidden>
        <button type="button" class="attach-photo">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="3" /><circle cx="8" cy="8" r="1.5" /><path d="m3 17 5-5 4 4 3-3 6 6" /></svg>
          Photo
        </button>
        <button type="button" class="attach-audio">
          <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M4 9v6M8 5v14M12 3v18M16 6v12M20 9v6" /></svg>
          Audio file
        </button>
      </div>
    </div>
    <label class="reply-mode">
      <span class="sr-only">Reply format</span>
        <select class="reply-select" aria-label="Reply format">
          <option value="text">Text</option>
          <option value="image">Image</option>
          <option value="audio">Audio</option>
        </select>
    </label>`;
  form.querySelector<HTMLElement>('.composer-bottom')!.prepend(tools);
  const draft = controls.querySelector<HTMLDivElement>('.draft-media')!;
  const attachmentControl = tools.querySelector<HTMLDivElement>('.attachment-control')!;
  const attachmentToggle = tools.querySelector<HTMLButtonElement>('.attachment-toggle')!;
  const attachmentMenu = tools.querySelector<HTMLDivElement>('.attachment-menu')!;
  const photo = tools.querySelector<HTMLButtonElement>('.attach-photo')!;
  const audio = tools.querySelector<HTMLButtonElement>('.attach-audio')!;
  const mode = tools.querySelector<HTMLSelectElement>('select')!;
  const voice = controls.querySelector<HTMLInputElement>('input')!;
  const voiceOption = controls.querySelector<HTMLLabelElement>('.voice-option')!;
  const notice = controls.querySelector<HTMLParagraphElement>('.media-notice')!;
  const textarea = form.querySelector<HTMLTextAreaElement>('textarea')!;
  const previews = createMediaRenderer(onError);
  let media: MediaAsset[] = [];
  let configuration: LlmConfiguration | null = null;
  let disabled = true;
  let importing = false;
  let disposed = false;
  let focusFrame = 0;
  let menuOpen = false;
  const nativePopover = typeof attachmentMenu.showPopover === 'function';
  if (nativePopover) attachmentMenu.setAttribute('popover', 'auto');

  function closeMenu(restoreFocus = false) {
    if (!menuOpen) return;
    menuOpen = false;
    if (nativePopover && attachmentMenu.matches(':popover-open')) attachmentMenu.hidePopover();
    attachmentMenu.hidden = true;
    attachmentToggle.setAttribute('aria-expanded', 'false');
    if (restoreFocus && !attachmentToggle.disabled) attachmentToggle.focus();
  }

  function positionMenu() {
    if (!nativePopover || !menuOpen) return;
    const anchor = attachmentToggle.getBoundingClientRect();
    const width = attachmentMenu.getBoundingClientRect().width;
    attachmentMenu.style.left = `${Math.max(12, Math.min(anchor.left, window.innerWidth - width - 12))}px`;
    attachmentMenu.style.top = `${Math.max(12, anchor.top - attachmentMenu.getBoundingClientRect().height - 8)}px`;
  }

  function openMenu() {
    if (attachmentToggle.disabled) return;
    menuOpen = true;
    attachmentMenu.hidden = false;
    attachmentToggle.setAttribute('aria-expanded', 'true');
    if (nativePopover) {
      attachmentMenu.style.position = 'fixed';
      attachmentMenu.style.margin = '0';
      attachmentMenu.style.bottom = 'auto';
      attachmentMenu.style.right = 'auto';
      attachmentMenu.style.maxWidth = 'calc(100vw - 24px)';
      attachmentMenu.showPopover();
      positionMenu();
    } else {
      attachmentMenu.style.position = 'static';
      controls.append(attachmentMenu);
    }
    (photo.disabled ? audio : photo).focus();
  }

  function outsideMenu(event: PointerEvent) {
    if (
      event.target instanceof Node &&
      !attachmentControl.contains(event.target) &&
      !attachmentMenu.contains(event.target)
    )
      closeMenu();
  }

  function menuKey(event: KeyboardEvent) {
    if (event.key !== 'Escape' || !menuOpen) return;
    event.preventDefault();
    event.stopPropagation();
    closeMenu(true);
  }

  function menuFocus(event: FocusEvent) {
    const next = event.relatedTarget;
    if (next instanceof Node && !attachmentControl.contains(next) && !attachmentMenu.contains(next))
      closeMenu();
  }

  function popoverToggle() {
    if (nativePopover && menuOpen && !attachmentMenu.matches(':popover-open')) closeMenu();
  }

  document.addEventListener('pointerdown', outsideMenu);
  document.addEventListener('keydown', menuKey);
  attachmentControl.addEventListener('focusout', menuFocus);
  attachmentMenu.addEventListener('focusout', menuFocus);
  attachmentMenu.addEventListener('toggle', popoverToggle);
  window.addEventListener('resize', positionMenu);
  function revealInput() {
    if (document.activeElement !== textarea) return;
    cancelAnimationFrame(focusFrame);
    focusFrame = requestAnimationFrame(() => {
      if (!disposed && document.activeElement === textarea) form.scrollTop = form.scrollHeight;
    });
  }
  textarea.addEventListener('focus', revealInput);
  window.addEventListener('resize', revealInput);

  function incompatibility(): string | undefined {
    const type = media.find(
      (attachment) => configuration?.capabilities[`${attachment.type}_input`] === 'unsupported',
    )?.type;
    if (type)
      return `The selected model cannot accept ${type} input. Choose another model or remove the attachment.`;
    if (mode.value === 'audio' && !/^[a-zA-Z0-9_-]{1,128}$/.test(voice.value.trim())) {
      return 'Enter a voice ID supported by the selected provider.';
    }
  }

  function update() {
    const busy = disabled || importing || !configuration;
    photo.disabled = busy || configuration?.capabilities.image_input === 'unsupported';
    audio.disabled = busy || configuration?.capabilities.audio_input === 'unsupported';
    attachmentToggle.disabled = busy || (photo.disabled && audio.disabled);
    if (attachmentToggle.disabled) closeMenu();
    mode.disabled = busy;
    voice.disabled = busy;
    for (const type of ['image', 'audio'] as const) {
      const option = mode.querySelector<HTMLOptionElement>(`option[value="${type}"]`)!;
      option.disabled =
        !configuration || configuration.capabilities[`${type}_output`] === 'unsupported';
      if (option.disabled && mode.value === type) mode.value = 'text';
    }
    voiceOption.hidden = mode.value !== 'audio';
    const invalid = incompatibility();
    notice.hidden = !media.length && !importing && !invalid;
    notice.textContent = importing
      ? 'Importing attachment…'
      : (invalid ??
        `Attachments will be sent to ${configuration?.provider_name ?? 'the selected provider'} · ${configuration?.model_id ?? ''}.`);
    for (const remove of draft.querySelectorAll<HTMLButtonElement>('.remove-attachment')) {
      remove.disabled = busy;
    }
  }

  function render() {
    previews.reset();
    draft.replaceChildren();
    draft.hidden = media.length === 0;
    for (const attachment of media) {
      const item = document.createElement('div');
      item.className = 'draft-attachment';
      if (attachment.type === 'image') {
        previews.render(item, [attachment]);
      } else {
        const label = document.createElement('span');
        label.className = 'media-label';
        label.textContent = attachment.file_name || 'Audio attachment';
        item.append(label);
      }
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'remove-attachment';
      remove.innerHTML =
        '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>';
      remove.setAttribute('aria-label', `Remove ${attachment.file_name ?? attachment.type}`);
      remove.addEventListener('click', () => {
        media = media.filter((value) => value.path !== attachment.path);
        render();
        onChange();
        void fs.remove(attachment.path).catch(onError);
      });
      item.append(remove);
      draft.append(item);
    }
    update();
  }

  async function attach(type: 'image' | 'audio') {
    closeMenu();
    if (disabled || importing || !configuration) return;
    if (media.length >= MAX_MEDIA_FILES) {
      onError(new Error('Attach up to 8 files per message.'));
      return;
    }
    importing = true;
    update();
    onChange();
    onError();
    let imported: MediaAsset | undefined;
    try {
      const asset =
        type === 'image'
          ? await photos.pick({ output_format: 'jpeg' })
          : await hostCall<MediaAsset | null>('audio.import');
      if (!asset) return;
      imported = { ...asset, type };
      const validated = await validateInputMedia(imported);
      if (disposed) return;
      if (
        media.reduce((size, value) => size + value.size_bytes, 0) + validated.size_bytes >
        MAX_MEDIA_BYTES
      ) {
        throw new Error('Attachments must total at most 8 MiB. Choose smaller files.');
      }
      media.push(validated);
      imported = undefined;
      render();
    } catch (error) {
      if (!disposed) onError(error);
    } finally {
      if (imported) void fs.remove(imported.path).catch(() => {});
      importing = false;
      update();
      onChange();
    }
  }

  photo.addEventListener('click', () => void attach('image'));
  audio.addEventListener('click', () => void attach('audio'));
  attachmentToggle.addEventListener('click', () => {
    if (menuOpen) closeMenu();
    else openMenu();
  });
  mode.addEventListener('change', () => {
    update();
    onChange();
  });
  voice.addEventListener('input', onChange);

  return {
    isBusy: () => importing,
    attachments: () => [...media],
    output(): LlmMediaOutput | undefined {
      if (mode.value === 'image') return { type: 'image', format: 'png' };
      if (mode.value === 'audio')
        return { type: 'audio', voice: voice.value.trim(), format: 'wav' };
      return undefined;
    },
    valid: () => !incompatibility(),
    update(disable: boolean, config: LlmConfiguration | null) {
      disabled = disable;
      configuration = config;
      update();
    },
    clearSent() {
      media = [];
      render();
    },
    draft(): ComposerDraft {
      return {
        media: media.map((asset) => ({ ...asset })),
        reply_mode: mode.value as ComposerDraft['reply_mode'],
        voice: voice.value,
      };
    },
    restoreDraft(value?: ComposerDraft) {
      closeMenu();
      media = value?.media.map((asset) => ({ ...asset })) ?? [];
      mode.value = value?.reply_mode ?? 'text';
      voice.value = value?.voice ?? 'alloy';
      render();
    },
    dispose() {
      disposed = true;
      closeMenu();
      cancelAnimationFrame(focusFrame);
      textarea.removeEventListener('focus', revealInput);
      window.removeEventListener('resize', revealInput);
      document.removeEventListener('pointerdown', outsideMenu);
      document.removeEventListener('keydown', menuKey);
      attachmentControl.removeEventListener('focusout', menuFocus);
      attachmentMenu.removeEventListener('focusout', menuFocus);
      attachmentMenu.removeEventListener('toggle', popoverToggle);
      window.removeEventListener('resize', positionMenu);
      previews.dispose();
    },
  };
}
