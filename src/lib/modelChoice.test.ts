import { describe, expect, it, vi } from 'vitest';
import { AppletModelChooser, publicSelection, type ModelChoice } from './modelChoice';
import type { LlmConnection, LlmSettings } from './types';

const connection: LlmConnection = {
  connection_id: 'connection_one',
  name: 'Private alias',
  provider_id: 'custom',
  base_url: 'https://private.invalid/v1',
  model_id: 'default',
  credential_status: 'absent',
};
const settings: LlmSettings = {
  active_connection_id: 'different-connection',
  connections: [connection],
  providers: [
    {
      provider_id: 'custom',
      name: 'Compatible provider',
      base_url: '',
      auth_methods: ['api_key', 'none'],
    },
  ],
};

describe('applet model chooser', () => {
  it('denies undeclared permissions and invalid applet-supplied targets before opening a sheet', async () => {
    const show = vi.fn();
    await expect(new AppletModelChooser([], show).open({})).rejects.toThrow('permission');
    const chooser = new AppletModelChooser(['llm'], show);
    for (const params of [
      null,
      [],
      { endpoint: 'private' },
      { current: { connection_id: '../other', model_id: 'model' } },
      { current: { connection_id: 'id', model_id: 'bad\nmodel' } },
      { current: { connection_id: 'id', model_id: 'model', provider_name: 'untrusted' } },
    ]) {
      await expect(chooser.open(params)).rejects.toThrow('Invalid');
    }
    expect(show).not.toHaveBeenCalled();
  });

  it('cancels a closed request, rejects concurrent sheets and ignores a late selection', async () => {
    let visible: ModelChoice | undefined;
    const chooser = new AppletModelChooser(['llm'], (choice) => {
      visible = choice;
    });
    const current = { connection_id: connection.connection_id, model_id: 'previous' };
    const pending = chooser.open({ current });
    const first = visible!;
    expect(first.current).toEqual(current);
    await expect(chooser.open({})).rejects.toThrow('already open');
    chooser.close();
    expect(await pending).toBeNull();
    const next = chooser.open({});
    const second = visible!;
    const selected = publicSelection(settings, connection, 'explicit-model')!;
    first.finish(selected);
    expect(visible).toBe(second);
    second.finish(selected);
    expect(await next).toEqual(selected);
    expect(visible).toBeUndefined();
  });

  it('returns the chosen model and preset label while keeping saved connection details private', () => {
    expect(publicSelection(settings, connection, ' manual/model:variant ')).toEqual({
      connection_id: 'connection_one',
      model_id: 'manual/model:variant',
      provider_id: 'custom',
      provider_name: 'Compatible provider',
    });
    expect(settings.active_connection_id).toBe('different-connection');
    expect(connection.model_id).toBe('default');
    expect(
      publicSelection(settings, { ...connection, provider_id: 'demo' }, 'model'),
    ).toBeUndefined();
    expect(
      publicSelection(settings, { ...connection, credential_status: 'unavailable' }, 'model'),
    ).toBeUndefined();
    expect(publicSelection(settings, connection, '模'.repeat(86))).toBeUndefined();
  });
});
