import { describe, expect, it, vi } from 'vitest';
import type { LlmConnection, LlmProvider, LlmSettings, LoginChallenge } from '../../lib/types';
import {
  changeProvider,
  connectionDraft,
  destinationChanged,
  hasChanges,
  matchingModels,
  OperationScope,
  savePayload,
  stopLogin,
} from './state';

const providers: LlmProvider[] = [
  {
    provider_id: 'openai',
    name: 'Compatible provider',
    base_url: 'https://provider.invalid/v1',
    auth_methods: ['api_key'],
  },
  {
    provider_id: 'codex',
    name: 'Account provider',
    base_url: 'https://account.invalid/api',
    auth_methods: ['device_code'],
  },
];
const connection: LlmConnection = {
  connection_id: 'saved_connection',
  name: 'Writing',
  provider_id: 'openai',
  base_url: 'https://provider.invalid/v1',
  model_id: 'model',
  credential_status: 'present',
};

describe('connection drafts', () => {
  it('preserves saved credentials unless an explicit replacement or removal is selected', () => {
    const draft = connectionDraft(connection, providers);
    expect(hasChanges(draft, connection)).toBe(false);
    expect(savePayload({ ...draft, api_key: 'unused-fixture' }, connection)).not.toHaveProperty(
      'api_key',
    );
    expect(
      savePayload({ ...draft, key_action: 'replace', api_key: 'new-fixture' }, connection),
    ).toMatchObject({ key_action: 'replace', api_key: 'new-fixture' });
    expect(
      savePayload({ ...draft, key_action: 'remove', api_key: 'unused-fixture' }, connection),
    ).not.toHaveProperty('api_key');
  });

  it('never carries an existing credential to another destination or provider', () => {
    const draft = connectionDraft(connection, providers);
    expect(
      savePayload({ ...draft, base_url: 'https://other.invalid/v1' }, connection),
    ).toMatchObject({ key_action: 'remove' });
    const switched = changeProvider(
      { ...draft, key_action: 'replace', api_key: 'typed-fixture' },
      providers[1],
    );
    expect(switched).toMatchObject({
      provider_id: 'codex',
      base_url: providers[1].base_url,
      api_key: '',
      model_id: '',
      key_action: 'remove',
    });
    expect(savePayload(switched, connection)).not.toHaveProperty('api_key');
  });

  it('keeps credentials for slash-only edits and migrated endpoints with trailing slashes', () => {
    for (const saved_url of [connection.base_url, `${connection.base_url}///`]) {
      const saved = { ...connection, base_url: saved_url };
      for (const draft_url of [
        connection.base_url,
        `${connection.base_url}/`,
        ` ${connection.base_url}/// `,
      ]) {
        const draft = { ...connectionDraft(saved, providers), base_url: draft_url };
        expect(destinationChanged(draft, saved)).toBe(false);
        expect(hasChanges(draft, saved)).toBe(false);
        expect(savePayload({ ...draft, name: 'Renamed connection' }, saved)).toMatchObject({
          connection_id: saved.connection_id,
          base_url: connection.base_url,
          name: 'Renamed connection',
          key_action: 'keep',
        });
      }
      const changed = {
        ...connectionDraft(saved, providers),
        base_url: `${connection.base_url}/other///`,
      };
      expect(destinationChanged(changed, saved)).toBe(true);
      expect(savePayload(changed, saved)).toMatchObject({
        base_url: `${connection.base_url}/other`,
        key_action: 'remove',
      });
    }
  });

  it('keeps manual model IDs and permits a new connection without choosing a model yet', () => {
    const draft = connectionDraft(undefined, providers);
    expect(savePayload(draft)).not.toHaveProperty('connection_id');
    expect(savePayload(draft).model_id).toBe('');
    expect(savePayload({ ...draft, model_id: ' namespace/manual:variant ' }).model_id).toBe(
      'namespace/manual:variant',
    );
    expect(
      matchingModels(
        [
          { model_id: 'namespace/model', name: 'Reasoning Model' },
          { model_id: 'other', name: 'Small' },
        ],
        'NAMESPACE',
      ),
    ).toHaveLength(1);
  });
});

describe('settings operation lifetime', () => {
  it('refreshes credential state only after a late-created login cancellation is acknowledged', async () => {
    let finish_begin!: (challenge: LoginChallenge) => void;
    const begin = new Promise<LoginChallenge>((resolve) => {
      finish_begin = resolve;
    });
    let acknowledge!: () => void;
    const cancellation = new Promise<void>((resolve) => {
      acknowledge = resolve;
    });
    let started!: () => void;
    const cancel_started = new Promise<void>((resolve) => {
      started = resolve;
    });
    const cancel = vi.fn(() => {
      started();
      return cancellation;
    });
    const settings: LlmSettings = {
      active_connection_id: 'saved_connection',
      connections: [connection],
      providers,
    };
    const refresh = vi.fn(async () => settings);
    const stopped = stopLogin(begin, cancel, refresh);
    expect(refresh).not.toHaveBeenCalled();
    finish_begin({
      login_id: 'login',
      user_code: 'code',
      verification_uri: 'https://provider.invalid/signin',
      expires_in: 600,
      interval: 5,
    });
    await cancel_started;
    expect(cancel).toHaveBeenCalledWith('login');
    expect(refresh).not.toHaveBeenCalled();
    acknowledge();
    expect(await stopped).toBe(settings);
    expect(refresh).toHaveBeenCalledOnce();
  });

  it('ignores late results without releasing a newer operation', () => {
    const scope = new OperationScope();
    const first = scope.start()!;
    expect(scope.start()).toBeUndefined();
    scope.cancel();
    const next = scope.start()!;
    expect(first.isCurrent()).toBe(false);
    first.finish();
    expect(next.isCurrent()).toBe(true);
    next.finish();
    expect(scope.start()).toBeDefined();
  });

  it('cancels a device login created after its editor was closed', async () => {
    const scope = new OperationScope();
    const operation = scope.start()!;
    const cancel = vi.fn();
    let resolve!: () => void;
    const begin = new Promise<void>((done) => {
      resolve = done;
    });
    const login = begin.then(() => operation.setCancellation(cancel));
    scope.cancel();
    resolve();
    await login;
    expect(cancel).toHaveBeenCalledOnce();
    expect(operation.isCurrent()).toBe(false);
  });

  it('cancels pending work once and never cancels successfully finished work', () => {
    const scope = new OperationScope();
    const first = scope.start()!;
    const cancel = vi.fn();
    first.setCancellation(cancel);
    scope.cancel();
    scope.cancel();
    expect(cancel).toHaveBeenCalledOnce();
    const next = scope.start()!;
    next.setCancellation(cancel);
    next.finish();
    scope.cancel();
    expect(cancel).toHaveBeenCalledOnce();
  });
});
