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

export function createComposerMedia(
  form: HTMLFormElement,
  onChange: () => void,
  onError: (error?: unknown) => void,
) {
  const controls = document.createElement('div');
  controls.className = 'composer-media';
  controls.innerHTML = `
    <div class="draft-media" aria-label="Attachments" hidden></div>
    <div class="media-options">
      <button type="button" class="attach-photo">Photo</button>
      <button type="button" class="attach-audio">Audio file</button>
      <label class="reply-mode">Reply
        <select aria-label="Reply format">
          <option value="text">Text</option>
          <option value="image">Image</option>
          <option value="audio">Audio</option>
        </select>
      </label>
      <label class="voice-option" hidden>Voice
        <input aria-label="Audio voice" value="alloy" maxlength="128" autocomplete="off" autocapitalize="none" spellcheck="false" />
      </label>
    </div>
    <p class="media-notice" role="status" hidden></p>`;
  form.prepend(controls);
  const draft = controls.querySelector<HTMLDivElement>('.draft-media')!;
  const photo = controls.querySelector<HTMLButtonElement>('.attach-photo')!;
  const audio = controls.querySelector<HTMLButtonElement>('.attach-audio')!;
  const mode = controls.querySelector<HTMLSelectElement>('select')!;
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
      remove.textContent = 'Remove';
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
    dispose() {
      disposed = true;
      cancelAnimationFrame(focusFrame);
      textarea.removeEventListener('focus', revealInput);
      window.removeEventListener('resize', revealInput);
      previews.dispose();
    },
  };
}
