import { beforeEach, expect, it, vi } from 'vitest';

const { read, stat } = vi.hoisted(() => ({ read: vi.fn(), stat: vi.fn() }));
vi.mock('../../../packages/applet-sdk/src', () => ({
  fs: { read, stat },
  hostCall: vi.fn(),
}));
import { MAX_MEDIA_BYTES, validateInputMedia } from './media';

beforeEach(() => {
  vi.clearAllMocks();
  stat.mockResolvedValue({ kind: 'file', size_bytes: 44 });
});

it('uses file bytes for audio type and strips native picker metadata before backend dispatch', async () => {
  read.mockResolvedValue({
    bytes: Array.from(new TextEncoder().encode('RIFF0000WAVE')),
    eof: true,
  });
  const imported = {
    type: 'audio' as const,
    path: 'recordings/fixture.wav',
    mime_type: 'application/octet-stream',
    size_bytes: 1,
    file_name: 'Fixture.wav',
    duration_ms: 1000,
  };
  expect(await validateInputMedia(imported)).toEqual({
    type: 'audio',
    path: imported.path,
    mime_type: 'audio/wav',
    size_bytes: 44,
    file_name: 'Fixture.wav',
  });
});

it('rejects unsupported M4A and HEIC attachments', async () => {
  read.mockResolvedValue({
    bytes: [0, 0, 0, 24, ...Array.from(new TextEncoder().encode('ftypM4A '))],
    eof: true,
  });
  await expect(
    validateInputMedia({
      type: 'audio',
      path: 'recordings/fixture.m4a',
      mime_type: 'audio/mp4',
      size_bytes: 44,
    }),
  ).rejects.toThrow('WAV or MP3');
  read.mockResolvedValue({
    bytes: [0, 0, 0, 24, ...Array.from(new TextEncoder().encode('ftypheic'))],
    eof: true,
  });
  await expect(
    validateInputMedia({
      type: 'image',
      path: 'photos/fixture.heic',
      mime_type: 'image/heic',
      size_bytes: 44,
    }),
  ).rejects.toThrow('JPEG, PNG, GIF, or WebP');
});

it('rejects large files before reading their bytes', async () => {
  stat.mockResolvedValue({ kind: 'file', size_bytes: MAX_MEDIA_BYTES + 1 });
  await expect(
    validateInputMedia({
      type: 'image',
      path: 'photos/large.png',
      mime_type: 'image/png',
      size_bytes: 1,
    }),
  ).rejects.toThrow('8 MiB');
  expect(read).not.toHaveBeenCalled();
});
