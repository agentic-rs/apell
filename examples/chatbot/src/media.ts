import { fs, hostCall } from '../../../packages/applet-sdk/src';

export interface MediaAsset {
  type: 'image' | 'audio';
  path: string;
  mime_type: string;
  size_bytes: number;
  file_name?: string;
  transcript?: string;
}

export const MAX_MEDIA_BYTES = 8 * 1024 * 1024;
const READ_BYTES = 256 * 1024;

/** Validate the stored bytes rather than trusting a picker's filename or MIME. */
export async function validateInputMedia(asset: MediaAsset): Promise<MediaAsset> {
  const stat = await fs.stat(asset.path);
  if (stat.kind !== 'file' || stat.size_bytes === 0 || stat.size_bytes > MAX_MEDIA_BYTES) {
    throw new Error('Choose a nonempty file no larger than 8 MiB.');
  }
  const { bytes } = await fs.read(asset.path, { length: 32 });
  const starts = (...signature: number[]) =>
    signature.every((byte, index) => bytes[index] === byte);
  const word = (offset: number, value: string) =>
    [...value].every((character, index) => bytes[offset + index] === character.charCodeAt(0));
  let mime_type: string | undefined;
  if (asset.type === 'image') {
    if (starts(0xff, 0xd8, 0xff)) mime_type = 'image/jpeg';
    else if (starts(0x89, 0x50, 0x4e, 0x47, 13, 10, 26, 10)) mime_type = 'image/png';
    else if (word(0, 'GIF87a') || word(0, 'GIF89a')) mime_type = 'image/gif';
    else if (word(0, 'RIFF') && word(8, 'WEBP')) mime_type = 'image/webp';
  } else if (word(0, 'RIFF') && word(8, 'WAVE')) {
    mime_type = 'audio/wav';
  } else {
    const id3 =
      word(0, 'ID3') &&
      bytes.length >= 10 &&
      bytes[3] >= 2 &&
      bytes[3] <= 4 &&
      bytes[4] !== 0xff &&
      bytes.slice(6, 10).every((byte) => byte < 0x80);
    const frame =
      bytes.length >= 4 &&
      bytes[0] === 0xff &&
      (bytes[1] & 0xe0) === 0xe0 &&
      (bytes[1] & 0x18) !== 0x08 &&
      (bytes[1] & 0x06) === 0x02 &&
      (bytes[2] & 0xf0) !== 0xf0 &&
      (bytes[2] & 0x0c) !== 0x0c;
    if (id3 || frame) mime_type = 'audio/mpeg';
  }
  if (!mime_type) {
    throw new Error(
      asset.type === 'image'
        ? 'Choose a JPEG, PNG, GIF, or WebP image.'
        : 'Choose a WAV or MP3 audio file.',
    );
  }
  return {
    type: asset.type,
    path: asset.path,
    mime_type,
    size_bytes: stat.size_bytes,
    ...(asset.file_name ? { file_name: asset.file_name } : {}),
  };
}

interface AudioStatus {
  playing: boolean;
  position_ms: number;
  duration_ms: number;
}

// Draft and message renderers share the one native player in this view.
const audio = {
  available: undefined as boolean | undefined,
  path: '',
  playing: false,
  ended: false,
  busy: false,
  version: 0,
  listeners: new Set<() => void>(),
  timer: undefined as ReturnType<typeof setTimeout> | undefined,
};
let capabilities: Promise<void> | undefined;
let audioOperation: Promise<void> | undefined;
let renderers = 0;

function updateAudio(): void {
  for (const listener of audio.listeners) listener();
}

function checkAudio(): void {
  capabilities ??= hostCall<{ playback: boolean }>('audio.capabilities')
    .then((value) => {
      audio.available = value.playback;
    })
    .catch(() => {
      audio.available = false;
    })
    .then(updateAudio);
}

function scheduleAudioStatus(): void {
  clearTimeout(audio.timer);
  if (!audio.playing || renderers === 0) return;
  const version = audio.version;
  audio.timer = setTimeout(() => {
    void hostCall<AudioStatus>('audio.status')
      .then((status) => {
        if (version !== audio.version) return;
        audio.playing = status.playing;
        audio.ended =
          !status.playing &&
          status.duration_ms > 0 &&
          status.position_ms >= status.duration_ms - 100;
        updateAudio();
        scheduleAudioStatus();
      })
      .catch(() => {
        if (version !== audio.version) return;
        audio.playing = false;
        updateAudio();
      });
  }, 600);
}

function toggleAudio(path: string): Promise<void> {
  if (audio.busy || !audio.available) return Promise.resolve();
  audio.busy = true;
  audio.version++;
  clearTimeout(audio.timer);
  updateAudio();
  const operation = (async () => {
    if (audio.path === path && audio.playing) {
      await hostCall('audio.playback_pause');
      audio.playing = false;
    } else {
      if (audio.path === path && audio.ended) await hostCall('audio.seek', { position_ms: 0 });
      await hostCall('audio.play', { path });
      audio.path = path;
      audio.playing = true;
      audio.ended = false;
    }
  })().finally(() => {
    if (audioOperation === operation) audioOperation = undefined;
    audio.busy = false;
    updateAudio();
    scheduleAudioStatus();
  });
  audioOperation = operation;
  return operation;
}

export async function pauseMediaPlayback(): Promise<void> {
  await audioOperation?.catch(() => {});
  if (!audio.playing) return;
  audio.busy = true;
  audio.version++;
  clearTimeout(audio.timer);
  updateAudio();
  try {
    await hostCall('audio.playback_pause');
    audio.playing = false;
  } finally {
    audio.busy = false;
    updateAudio();
    scheduleAudioStatus();
  }
}

interface ImageView {
  node: HTMLElement;
  image: HTMLImageElement;
  status: HTMLElement;
  asset: MediaAsset;
  visible: boolean;
  loading: boolean;
  version: number;
  url?: string;
}

/** Own the asynchronous media and object URLs for one rerenderable region. */
export function createMediaRenderer(onError: (error: unknown) => void) {
  renderers++;
  let disposed = false;
  let loads = 0;
  const images = new Map<HTMLElement, ImageView>();
  const audioListeners = new Set<() => void>();
  const queue: ImageView[] = [];

  function releaseImage(view: ImageView): void {
    view.version++;
    if (view.url && view.node.isConnected) {
      view.node.style.minHeight = `${view.node.getBoundingClientRect().height}px`;
    }
    if (view.url) URL.revokeObjectURL(view.url);
    view.url = undefined;
    view.image.removeAttribute('src');
    view.image.hidden = true;
    view.status.hidden = false;
  }

  async function loadImage(view: ImageView): Promise<void> {
    const version = view.version;
    const current = () =>
      !disposed && view.visible && view.node.isConnected && version === view.version;
    const chunks: ArrayBuffer[] = [];
    let offset = 0;
    while (current()) {
      const result = await fs.read(view.asset.path, { offset, length: READ_BYTES });
      if (!current()) return;
      if (offset + result.bytes.length > MAX_MEDIA_BYTES)
        throw new Error('This image exceeds 8 MiB.');
      chunks.push(new Uint8Array(result.bytes).buffer);
      offset += result.bytes.length;
      if (result.eof) break;
      if (result.bytes.length === 0) throw new Error('The image could not be read.');
    }
    if (!current()) return;
    const url = URL.createObjectURL(new Blob(chunks, { type: view.asset.mime_type }));
    const image = view.image.cloneNode(false) as HTMLImageElement;
    image.addEventListener('error', () => {
      if (!current() || view.image !== image || view.url !== url) return;
      view.status.textContent = 'Image unavailable';
      releaseImage(view);
    });
    image.addEventListener('load', () => {
      if (current() && view.image === image) view.node.style.minHeight = '';
    });
    view.image.replaceWith(image);
    view.image = image;
    view.url = url;
    image.src = url;
    image.hidden = false;
    view.status.hidden = true;
  }

  function pump(): void {
    while (!disposed && loads < 2 && queue.length) {
      const view = queue.shift()!;
      if (!view.visible || view.loading || view.url || !images.has(view.node)) continue;
      view.loading = true;
      const version = view.version;
      loads++;
      void loadImage(view)
        .catch((error) => {
          if (!disposed && view.visible && version === view.version && images.has(view.node)) {
            view.status.textContent = 'Image unavailable';
            onError(error);
          }
        })
        .finally(() => {
          view.loading = false;
          loads--;
          if (view.visible && version !== view.version && images.has(view.node)) queue.push(view);
          pump();
        });
    }
  }

  const observer =
    typeof IntersectionObserver === 'undefined'
      ? undefined
      : new IntersectionObserver(
          (entries) => {
            for (const entry of entries) {
              const view = images.get(entry.target as HTMLElement);
              if (!view) continue;
              view.visible = entry.isIntersecting;
              if (view.visible) queue.push(view);
              else releaseImage(view);
            }
            pump();
          },
          { rootMargin: '100px' },
        );

  function render(target: HTMLElement, media: readonly MediaAsset[]): void {
    if (disposed || media.length === 0) return;
    const list = document.createElement('div');
    list.className = 'media-list';
    for (const asset of media) {
      const item = document.createElement('div');
      item.className = asset.type === 'image' ? 'media-item' : 'media-item media-audio';
      const label = document.createElement('span');
      label.className = 'media-label';
      label.textContent = asset.file_name || (asset.type === 'image' ? 'Image' : 'Audio');
      item.appendChild(label);
      if (asset.type === 'image') {
        const image = document.createElement('img');
        image.className = 'media-image';
        image.alt = asset.file_name || 'Attached image';
        image.hidden = true;
        const status = document.createElement('span');
        status.className = 'media-status';
        status.textContent = 'Loading image…';
        const view: ImageView = {
          node: item,
          image,
          status,
          asset,
          visible: false,
          loading: false,
          version: 0,
        };
        item.append(image, status);
        images.set(item, view);
        if (observer) observer.observe(item);
        else {
          view.visible = true;
          queue.push(view);
        }
      } else {
        checkAudio();
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'media-play';
        const status = document.createElement('span');
        status.className = 'media-status';
        const update = () => {
          const playing = audio.path === asset.path && audio.playing;
          button.textContent = playing ? 'Pause' : 'Play';
          button.setAttribute(
            'aria-label',
            `${playing ? 'Pause' : 'Play'} ${asset.file_name || 'audio'}`,
          );
          button.disabled = !audio.available || audio.busy;
          status.textContent = '';
          if (audio.available === false)
            status.textContent = 'Playback is available on iOS and Android.';
          else if (audio.available === undefined) status.textContent = 'Checking playback…';
          else if (audio.path === asset.path)
            status.textContent = playing ? 'Playing' : audio.ended ? 'Finished' : 'Paused';
        };
        button.addEventListener('click', () => {
          void toggleAudio(asset.path).catch((error) => {
            if (!disposed) onError(error);
          });
        });
        audio.listeners.add(update);
        audioListeners.add(update);
        update();
        item.append(button, status);
      }
      list.appendChild(item);
    }
    target.appendChild(list);
    if (!observer) queueMicrotask(pump);
  }

  function reset(): void {
    observer?.disconnect();
    for (const view of images.values()) releaseImage(view);
    images.clear();
    queue.length = 0;
    for (const listener of audioListeners) audio.listeners.delete(listener);
    audioListeners.clear();
  }

  function dispose(): void {
    if (disposed) return;
    disposed = true;
    reset();
    renderers--;
    if (renderers === 0) {
      audio.version++;
      clearTimeout(audio.timer);
      if (audio.playing) void hostCall('audio.playback_pause').catch(() => {});
      audio.playing = false;
    }
    window.removeEventListener('pagehide', dispose);
  }

  window.addEventListener('pagehide', dispose, { once: true });
  return { render, reset, dispose };
}
