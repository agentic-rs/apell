import { describe, expect, it, vi } from 'vitest';
import type { LlmConnection, LlmProvider, NativeConnectionDraft } from '../../lib/types';
import {
  canSaveDraft,
  changeProvider,
  connectionDraft,
  credentialPayload,
  destinationChanged,
  draftPayload,
  DraftSession,
  hasChanges,
  matchingModels,
  OperationScope,
  savePayload,
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
const native_draft: NativeConnectionDraft = { draft_id: 'temporary', credential_status: 'absent' };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe('connection setup', () => {
  it('loads models using a request-only replacement before the connection is saved', () => {
    const draft = { ...connectionDraft(undefined, providers), api_key: 'typed-fixture' };
    expect(draftPayload(draft)).toEqual({ provider_id: 'openai', base_url: connection.base_url });
    expect(draftPayload(draft)).not.toHaveProperty('api_key');
    expect(draftPayload(draft)).not.toHaveProperty('model_id');
    expect(credentialPayload(draft)).toEqual({ key_action: 'replace', api_key: 'typed-fixture' });
    expect(() => savePayload(draft, native_draft.draft_id, providers[0].name)).toThrow(
      'Choose a model',
    );
    expect(canSaveDraft(draft, providers[0])).toBe(false);
    const selected = { ...draft, model_id: ' namespace/manual:variant ', name: '' };
    expect(canSaveDraft(selected, providers[0])).toBe(true);
    expect(savePayload(selected, native_draft.draft_id, providers[0].name)).toEqual({
      draft_id: 'temporary',
      name: providers[0].name,
      model_id: 'namespace/manual:variant',
      key_action: 'replace',
      api_key: 'typed-fixture',
    });
  });

  it('keeps staged OAuth when saving a different destination and marks same-provider re-login dirty', () => {
    const draft = {
      ...changeProvider(connectionDraft(connection, providers), providers[1]),
      model_id: 'model',
    };
    expect(canSaveDraft(draft, providers[1], connection)).toBe(false);
    expect(canSaveDraft(draft, providers[1], connection, true)).toBe(true);
    expect(savePayload(draft, 'authenticated-draft', providers[1].name)).toMatchObject({
      key_action: 'keep',
    });
    const saved = { ...connection, provider_id: 'codex', base_url: providers[1].base_url };
    expect(hasChanges(connectionDraft(saved, providers), saved)).toBe(false);
    expect(hasChanges(connectionDraft(saved, providers), saved, true)).toBe(true);
  });

  it('requires fresh authentication after removing a saved key and changing its destination', () => {
    const draft = {
      ...connectionDraft(connection, providers),
      remove_credential: true,
      base_url: 'https://other.invalid/v1',
    };
    expect(canSaveDraft(draft, providers[0], connection)).toBe(false);
    expect(
      canSaveDraft(
        { ...draft, remove_credential: false, api_key: 'new-fixture' },
        providers[0],
        connection,
      ),
    ).toBe(true);
    expect(
      canSaveDraft(draft, { ...providers[0], auth_methods: ['api_key', 'none'] }, connection),
    ).toBe(true);
  });

  it('preserves inherited credentials when replacement is cleared or removal is undone', () => {
    for (const credential_status of ['present', 'unavailable'] as const) {
      const saved = { ...connection, credential_status };
      const draft = connectionDraft(saved, providers);
      expect(hasChanges(draft, saved)).toBe(false);
      expect(canSaveDraft(draft, providers[0], saved)).toBe(true);
      const replacement = { ...draft, api_key: 'new-fixture' };
      expect(credentialPayload(replacement)).toEqual({
        key_action: 'replace',
        api_key: 'new-fixture',
      });
      const cleared = { ...replacement, api_key: '' };
      expect(credentialPayload(cleared)).toEqual({ key_action: 'keep' });
      expect(hasChanges(cleared, saved)).toBe(false);
      const removal = { ...cleared, remove_credential: true };
      expect(credentialPayload(removal)).toEqual({ key_action: 'remove' });
      expect(canSaveDraft(removal, providers[0], saved)).toBe(true);
      expect(hasChanges({ ...removal, remove_credential: false }, saved)).toBe(false);
    }
  });

  it('normalizes slash-only endpoint edits while sending source and destination separately', () => {
    for (const base_url of [connection.base_url, `${connection.base_url}///`]) {
      const saved = { ...connection, base_url };
      for (const draft_url of [
        connection.base_url,
        `${connection.base_url}/`,
        ` ${connection.base_url}/// `,
      ]) {
        const draft = { ...connectionDraft(saved, providers), base_url: draft_url };
        expect(destinationChanged(draft, saved)).toBe(false);
        expect(hasChanges(draft, saved)).toBe(false);
        expect(draftPayload(draft, saved)).toEqual({
          connection_id: saved.connection_id,
          provider_id: saved.provider_id,
          base_url: connection.base_url,
        });
        expect(credentialPayload(draft)).toEqual({ key_action: 'keep' });
      }
    }
    const changed = changeProvider(
      { ...connectionDraft(connection, providers), api_key: 'typed-fixture' },
      providers[1],
    );
    expect(changed.api_key).toBe('');
    expect(changed.model_id).toBe('');
    expect(destinationChanged(changed, connection)).toBe(true);
    expect(draftPayload(changed, connection)).toMatchObject({
      connection_id: connection.connection_id,
      provider_id: 'codex',
    });
  });

  it('keeps optional credentials and manual model selection available', () => {
    const provider: LlmProvider = {
      ...providers[0],
      provider_id: 'custom',
      auth_methods: ['api_key', 'none'],
    };
    const draft = {
      ...changeProvider(connectionDraft(undefined, providers), provider),
      model_id: 'custom-model',
    };
    expect(canSaveDraft(draft, provider)).toBe(true);
    expect(credentialPayload(draft)).toEqual({ key_action: 'keep' });
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

describe('temporary native drafts', () => {
  it('discards a late-created draft after Back without exposing it to the abandoned operation', async () => {
    const pending = deferred<NativeConnectionDraft>();
    const remove = vi.fn(async () => {});
    const session = new DraftSession(() => pending.promise, remove);
    const setup = session.ensure(draftPayload(connectionDraft(undefined, providers)));
    const rejected = expect(setup).rejects.toThrow('no longer active');
    const discarded = session.discard();
    pending.resolve(native_draft);
    await discarded;
    await rejected;
    expect(remove).toHaveBeenCalledExactlyOnceWith('temporary');
  });

  it('reuses auth across metadata and slash edits, retains failed-save drafts, and never discards consumed auth', async () => {
    const create = vi.fn(async () => native_draft);
    const remove = vi.fn(async () => {});
    const session = new DraftSession(create, remove);
    const draft = connectionDraft(connection, providers);
    await session.ensure(draftPayload(draft, connection));
    session.update({ ...native_draft, credential_status: 'present' });
    const edited = {
      ...draft,
      name: 'Renamed',
      model_id: 'another-model',
      base_url: `${draft.base_url}///`,
    };
    expect((await session.ensure(draftPayload(edited, connection))).credential_status).toBe(
      'present',
    );
    // A rejected save does not call consume; retry uses the same authenticated draft.
    expect(await session.ensure(draftPayload(edited, connection))).toMatchObject({
      draft_id: 'temporary',
    });
    expect(create).toHaveBeenCalledOnce();
    session.consume('temporary');
    await session.discard();
    expect(remove).not.toHaveBeenCalled();
  });

  it('discards old destination auth and keeps a newer draft when the old creation completes late', async () => {
    const first = deferred<NativeConnectionDraft>();
    const create = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce({ draft_id: 'next', credential_status: 'absent' });
    const remove = vi.fn(async () => {});
    const session = new DraftSession(create, remove);
    const setup = session.ensure(draftPayload(connectionDraft(undefined, providers)));
    const rejected = expect(setup).rejects.toThrow('no longer active');
    const cleanup = session.discard();
    const next_input = draftPayload(
      changeProvider(connectionDraft(undefined, providers), providers[1]),
    );
    const next = await session.ensure(next_input);
    first.resolve(native_draft);
    await cleanup;
    await rejected;
    expect(await session.ensure(next_input)).toEqual(next);
    expect(remove).toHaveBeenCalledExactlyOnceWith('temporary');
    await session.discard();
    expect(remove).toHaveBeenLastCalledWith('next');
  });

  it('propagates discard failure without handing out the abandoned auth again', async () => {
    const create = vi
      .fn()
      .mockResolvedValueOnce(native_draft)
      .mockResolvedValueOnce({ draft_id: 'fresh', credential_status: 'absent' });
    const session = new DraftSession(create, async () => {
      throw new Error('cleanup failed');
    });
    const input = draftPayload(connectionDraft(undefined, providers));
    await session.ensure(input);
    await expect(session.discard()).rejects.toThrow('cleanup failed');
    expect((await session.ensure(input)).draft_id).toBe('fresh');
  });
});

describe('settings operation lifetime', () => {
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

  it('cancels a device login created after its native draft and editor were discarded', async () => {
    const scope = new OperationScope();
    const operation = scope.start()!;
    const cancel = vi.fn();
    const begin = deferred<string>();
    const login = begin.promise.then((id) => operation.setCancellation(() => cancel(id)));
    scope.cancel();
    begin.resolve('late-login');
    await login;
    expect(cancel).toHaveBeenCalledExactlyOnceWith('late-login');
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
