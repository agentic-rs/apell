import { describe, expect, it, vi } from 'vitest';
import {
  AppletModelChooser,
  connectionReadiness,
  connectionReadinessAction,
  connectionReadinessLabel,
  initialModelTarget,
  publicSelection,
  type ModelChoice,
} from './modelChoice';
import type { LlmConnection, LlmProvider, LlmSettings } from './types';

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
  pending_preferred_connection_id: null,
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

describe('saved connection readiness', () => {
  const providers: LlmProvider[] = [
    ...settings.providers,
    { provider_id: 'openai', name: 'OpenAI', base_url: '', auth_methods: ['api_key'] },
    { provider_id: 'codex', name: 'Codex', base_url: '', auth_methods: ['device_code'] },
  ];

  it('allows an absent optional key but blocks all unreadable credentials', () => {
    expect(connectionReadiness(connection, providers).state).toBe('ready');
    const unavailable = connectionReadiness(
      { ...connection, credential_status: 'unavailable' },
      providers,
    );
    expect(unavailable.state).toBe('credential_unavailable');
    expect(connectionReadinessLabel(unavailable)).toBe('Credential unavailable');
    expect(connectionReadinessAction(unavailable)).toBe('Edit connection');
    expect(
      connectionReadiness({ ...connection, provider_id: 'missing-provider' }, providers).state,
    ).toBe('provider_unavailable');
  });

  it('distinguishes missing provider authentication from a missing saved model', () => {
    const without_key = connectionReadiness({ ...connection, provider_id: 'openai' }, providers);
    expect(without_key.state).toBe('needs_api_key');
    expect(connectionReadinessAction(without_key)).toBe('Add API key');
    const without_sign_in = connectionReadiness(
      { ...connection, provider_id: 'codex', model_id: '' },
      providers,
    );
    expect(without_sign_in.state).toBe('needs_sign_in');
    expect(connectionReadinessAction(without_sign_in)).toBe('Sign in');
    for (const model_id of ['', '   ', 'bad\nmodel', '模'.repeat(86)]) {
      expect(connectionReadiness({ ...connection, model_id }, providers).state).toBe('needs_model');
    }
  });

  it('permits a manual applet override for a connection that needs a saved model', () => {
    const without_model = { ...connection, model_id: '' };
    expect(connectionReadiness(without_model, providers).state).toBe('needs_model');
    expect(publicSelection(settings, without_model, 'manual-model')).toMatchObject({
      connection_id: without_model.connection_id,
      model_id: 'manual-model',
    });
  });
});

describe('initial applet model target', () => {
  const default_connection: LlmConnection = {
    ...connection,
    connection_id: 'default-connection',
    model_id: 'preferred-model',
  };
  const picker_settings = {
    ...settings,
    active_connection_id: default_connection.connection_id,
    connections: [connection, default_connection],
  };

  it('keeps an explicit prior choice even when the connection is unavailable', () => {
    const prior = { connection_id: connection.connection_id, model_id: 'manual-choice' };
    expect(
      initialModelTarget(
        {
          ...picker_settings,
          connections: [{ ...connection, credential_status: 'unavailable' }, default_connection],
        },
        prior,
      ),
    ).toEqual(prior);
  });

  it('prefers a ready default, falling back to a usable connection without changing it', () => {
    expect(initialModelTarget(picker_settings)?.connection_id).toBe(
      default_connection.connection_id,
    );
    const blocked = { ...default_connection, credential_status: 'unavailable' as const };
    expect(
      initialModelTarget({ ...picker_settings, connections: [connection, blocked] })?.connection_id,
    ).toBe(connection.connection_id);
    expect(
      initialModelTarget({
        ...picker_settings,
        connections: [blocked, { ...connection, model_id: '' }],
      }),
    ).toEqual({ connection_id: connection.connection_id, model_id: '' });
    expect(initialModelTarget({ ...picker_settings, connections: [blocked] })?.connection_id).toBe(
      blocked.connection_id,
    );
  });
});

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
      publicSelection(settings, { ...connection, provider_id: 'missing-provider' }, 'model'),
    ).toBeUndefined();
    expect(
      publicSelection(settings, { ...connection, credential_status: 'unavailable' }, 'model'),
    ).toBeUndefined();
    expect(publicSelection(settings, connection, '模'.repeat(86))).toBeUndefined();
  });
});
