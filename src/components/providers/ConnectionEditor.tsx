import { useEffect, useRef, useState } from 'react';
import {
  activateConnection,
  beginLogin,
  cancelConnectionTest,
  cancelLogin,
  createConnectionDraft,
  deleteConnection,
  discardConnectionDraft,
  discoverDraftModels,
  message,
  openLogin,
  pollLogin,
  saveConnection,
  testConnection,
} from '../../lib/tauri';
import type {
  LlmConnection,
  LlmModels,
  LlmProvider,
  LoginChallenge,
  NativeConnectionDraft,
} from '../../lib/types';
import { Icon } from '../Icon';
import { ApiKeyField } from './ApiKeyField';
import { ModelPicker } from './ModelPicker';
import {
  changeProvider,
  canSaveDraft,
  connectionDraft,
  credentialPayload,
  destinationChanged,
  draftPayload,
  DraftSession,
  hasChanges,
  OperationScope,
  requestId,
  savePayload,
  type Operation,
} from './state';

interface EditorProps {
  connection?: LlmConnection;
  providers: LlmProvider[];
  active_connection_id: string;
  onSaved: (connection: LlmConnection, previous_id?: string) => void;
  onActivated: (connection_id: string) => void;
  onDeleted: (connection_id: string) => void;
  onBack: () => void;
}

export function ConnectionEditor(props: EditorProps) {
  const [saved, setSaved] = useState(props.connection);
  const [draft, setDraft] = useState(() => connectionDraft(props.connection, props.providers));
  const [models, setModels] = useState<LlmModels>();
  const [login, setLogin] = useState<LoginChallenge>();
  const [native_draft, setNativeDraft] = useState<NativeConnectionDraft>();
  const [staged_login, setStagedLogin] = useState(false);
  const [busy, setBusy] = useState('');
  const [status, setStatus] = useState('');
  const [failed, setFailed] = useState(false);
  const [confirm_delete, setConfirmDelete] = useState(false);
  const scope = useRef(new OperationScope());
  const drafts = useRef(new DraftSession(createConnectionDraft, discardConnectionDraft));
  const login_operation = useRef<Operation | undefined>(undefined);
  const provider = props.providers.find((item) => item.provider_id === draft.provider_id);
  const is_demo = draft.provider_id === 'demo';
  const device_login = provider?.auth_methods.includes('device_code') ?? false;
  const accepts_key = provider?.auth_methods.includes('api_key') ?? false;
  const changed_destination = destinationChanged(draft, saved);
  const dirty = hasChanges(draft, saved, staged_login);
  const can_probe = !!saved && !dirty && !busy;
  const active = saved?.connection_id === props.active_connection_id;
  const inherited_status = !changed_destination ? saved?.credential_status : undefined;
  const credential_status = native_draft?.credential_status ?? inherited_status;
  const can_discover =
    !busy &&
    !!provider &&
    !!draft.base_url.trim() &&
    (provider.auth_methods.includes('none') ||
      (!draft.remove_credential &&
        (!!draft.api_key ||
          credential_status === 'present' ||
          credential_status === 'unavailable')));

  useEffect(
    () => () => {
      scope.current.cancel();
      void drafts.current.discard().catch(() => {});
    },
    [],
  );

  const announce = (text: string, error = false) => {
    setStatus(text);
    setFailed(error);
  };
  const acceptSaved = (connection: LlmConnection) => {
    props.onSaved(connection, saved?.connection_id);
    setSaved(connection);
    setDraft(connectionDraft(connection, props.providers));
    setNativeDraft(undefined);
    setStagedLogin(false);
  };
  const run = async (kind: string, action: (operation: Operation) => Promise<void>) => {
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(kind);
    setConfirmDelete(false);
    announce('');
    try {
      await action(operation);
    } catch (error) {
      if (operation.isCurrent()) announce(message(error), true);
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setBusy('');
        setLogin(undefined);
        login_operation.current = undefined;
      }
    }
  };
  const cancel = () => {
    const was_login = busy === 'login';
    scope.current.cancel();
    login_operation.current = undefined;
    setBusy('');
    setLogin(undefined);
    if (!was_login) {
      announce('Test cancelled.');
      return;
    }
    void run('cancel_login', async (operation) => {
      const disposal = drafts.current.discard();
      setNativeDraft(undefined);
      setStagedLogin(false);
      setModels(undefined);
      await disposal;
      if (!operation.isCurrent()) return;
      announce('Sign-in cancelled. No credential was saved.');
    });
  };
  const resetDestination = () => {
    scope.current.cancel();
    void drafts.current.discard().catch(() => {});
    setNativeDraft(undefined);
    setStagedLogin(false);
    setModels(undefined);
    setLogin(undefined);
  };
  const ensureDraft = async (operation: Operation) => {
    const value = await drafts.current.ensure(draftPayload(draft, saved));
    if (operation.isCurrent()) setNativeDraft(value);
    return value;
  };
  const loadModels = async (
    operation: Operation,
    value: NativeConnectionDraft,
    signed_in = false,
  ) => {
    const result = await discoverDraftModels(
      value.draft_id,
      signed_in ? { key_action: 'keep' } : credentialPayload(draft),
    );
    if (!operation.isCurrent()) return;
    setModels(result);
    announce(
      result.models.length
        ? `${signed_in ? 'Signed in for this setup. ' : ''}Choose a model, then save.`
        : `${signed_in ? 'Signed in for this setup. ' : ''}No model list is available. Enter a model ID, then save.`,
    );
  };
  const signIn = () =>
    void run('login', async (operation) => {
      const value = await ensureDraft(operation);
      if (!operation.isCurrent()) return;
      const challenge = await beginLogin(value.draft_id);
      operation.setCancellation(() => {
        void cancelLogin(challenge.login_id).catch(() => {});
      });
      if (!operation.isCurrent()) return;
      login_operation.current = operation;
      setLogin(challenge);
      const authenticated = await pollLogin(challenge.login_id);
      if (!operation.isCurrent()) return;
      drafts.current.update(authenticated);
      setNativeDraft(authenticated);
      setStagedLogin(true);
      setDraft((current) => ({ ...current, remove_credential: false, api_key: '' }));
      setLogin(undefined);
      setBusy('models');
      try {
        await loadModels(operation, authenticated, true);
      } catch (error) {
        if (operation.isCurrent()) announce(`Signed in for this setup. ${message(error)}`, true);
      }
    });
  const openSignIn = async () => {
    const operation = login_operation.current;
    if (!login || !operation?.isCurrent()) return;
    try {
      await openLogin(login.login_id);
    } catch (error) {
      if (operation.isCurrent()) announce(message(error), true);
    }
  };

  return (
    <section className="settings connection-editor panel" aria-busy={!!busy}>
      <div className="settings-heading">
        <button
          type="button"
          className="text-button"
          disabled={['save', 'activate', 'delete'].includes(busy)}
          onClick={props.onBack}
        >
          <Icon name="back" /> Connections
        </button>
        {active && <span className="connection-badge">Active</span>}
      </div>
      <h2>{saved ? saved.name : 'Add connection'}</h2>
      <p>
        {active
          ? 'New requests use this connection.'
          : 'Choose a provider, connect your account, and select a model.'}
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void run('save', async (operation) => {
            const value = await ensureDraft(operation);
            if (!operation.isCurrent()) return;
            const connection = await saveConnection(
              savePayload(draft, value.draft_id, provider?.name ?? draft.provider_id),
            );
            drafts.current.consume(value.draft_id);
            if (!operation.isCurrent()) return;
            acceptSaved(connection);
            if (changed_destination) setModels(undefined);
            announce('Connection saved.');
          });
        }}
      >
        <fieldset disabled={!!busy}>
          <label>
            Provider
            <select
              value={draft.provider_id}
              disabled={saved?.provider_id === 'demo'}
              onChange={(event) => {
                const next = props.providers.find(
                  (item) => item.provider_id === event.target.value,
                );
                if (!next) return;
                resetDestination();
                setDraft({
                  ...changeProvider(draft, next),
                  name: draft.name === provider?.name ? next.name : draft.name,
                });
                announce('');
              }}
            >
              {props.providers
                .filter((item) => item.provider_id !== 'demo' || saved?.provider_id === 'demo')
                .map((item) => (
                  <option key={item.provider_id} value={item.provider_id}>
                    {item.name}
                  </option>
                ))}
              {!provider && (
                <option value={draft.provider_id}>{draft.provider_id} (unavailable)</option>
              )}
            </select>
          </label>
          {is_demo ? (
            <div className="notice">
              Local demo returns scripted replies without sending a model request.
            </div>
          ) : (
            <>
              {!device_login && (
                <details
                  className="connection-advanced"
                  open={draft.provider_id === 'custom' || !draft.base_url || undefined}
                >
                  <summary>API endpoint</summary>
                  <label>
                    Base URL
                    <input
                      type="url"
                      required
                      value={draft.base_url}
                      autoCapitalize="none"
                      autoCorrect="off"
                      spellCheck={false}
                      placeholder="https://api.example.com/v1"
                      onChange={(event) => {
                        const next = { ...draft, base_url: event.target.value };
                        if (
                          draftPayload(next, saved).base_url !== draftPayload(draft, saved).base_url
                        )
                          resetDestination();
                        setDraft(next);
                      }}
                    />
                  </label>
                </details>
              )}
              {accepts_key && (
                <ApiKeyField
                  credential_status={credential_status}
                  optional={provider?.auth_methods.includes('none') ?? false}
                  value={draft.api_key}
                  removing={draft.remove_credential}
                  onChange={(api_key) => {
                    setDraft({ ...draft, api_key, remove_credential: false });
                    setModels(undefined);
                  }}
                  onRemove={() => {
                    setDraft({ ...draft, remove_credential: true, api_key: '' });
                    setModels(undefined);
                  }}
                  onUndo={() => {
                    setDraft({ ...draft, remove_credential: false });
                    setModels(undefined);
                  }}
                />
              )}
              {device_login && (
                <div className="credential-fields">
                  <p className="status">
                    {staged_login
                      ? draft.remove_credential
                        ? 'This sign-in will be discarded when you save.'
                        : 'Signed in for this setup. Save to keep this account.'
                      : credential_status === 'present'
                        ? 'Signed in securely.'
                        : credential_status === 'unavailable'
                          ? 'The secure credential store is unavailable.'
                          : 'Sign in with your account using a one-time code.'}
                  </p>
                  <button type="button" disabled={!!busy} onClick={signIn}>
                    {credential_status === 'present' ? 'Sign in again' : 'Sign in'}
                  </button>
                  {credential_status === 'present' && (
                    <button
                      type="button"
                      className={`text-button${draft.remove_credential ? '' : ' danger'}`}
                      onClick={() => {
                        setDraft({
                          ...draft,
                          remove_credential: !draft.remove_credential,
                          api_key: '',
                        });
                        setModels(undefined);
                        announce(draft.remove_credential ? '' : 'Save to remove the sign-in.');
                      }}
                    >
                      {draft.remove_credential ? 'Undo removal' : 'Remove sign-in'}
                    </button>
                  )}
                </div>
              )}
            </>
          )}
        </fieldset>
        {login && (
          <div className="device-login" role="region" aria-label="Device sign-in">
            <p>Enter this code on the sign-in page:</p>
            <code>{login.user_code}</code>
            <p className="status">{login.verification_uri}</p>
            <p className="status">
              The code expires in about {Math.max(1, Math.ceil(login.expires_in / 60))} minutes.
              Return here after signing in.
            </p>
            <button type="button" className="primary" onClick={() => void openSignIn()}>
              Open sign-in page
            </button>
            <p className="status" role="status">
              Waiting for sign-in…
            </p>
          </div>
        )}
        {busy === 'login' && !login && (
          <p className="status connection-status" role="status">
            Requesting a sign-in code…
          </p>
        )}
        {busy === 'cancel_login' && (
          <p className="status connection-status" role="status">
            Stopping sign-in…
          </p>
        )}
        {busy === 'login' && (
          <button type="button" className="cancel-operation" onClick={cancel}>
            Cancel sign-in
          </button>
        )}
        {!is_demo && (
          <fieldset disabled={!!busy}>
            <ModelPicker
              model_id={draft.model_id}
              models={models}
              disabled={!!busy}
              required
              onChange={(model_id) => setDraft({ ...draft, model_id })}
            />
            <button
              type="button"
              disabled={!can_discover}
              onClick={() =>
                void run('models', async (operation) => {
                  const value = await ensureDraft(operation);
                  if (!operation.isCurrent()) return;
                  await loadModels(operation, value);
                })
              }
            >
              {busy === 'models' ? 'Loading models…' : models ? 'Refresh models' : 'Load models'}
            </button>
            <label>
              Name (optional)
              <input
                maxLength={80}
                value={draft.name}
                placeholder={provider?.name}
                autoComplete="off"
                onChange={(event) => setDraft({ ...draft, name: event.target.value })}
              />
            </label>
            {changed_destination && (
              <p className="status">
                This is a different destination. The previous credential will not be reused.
              </p>
            )}
          </fieldset>
        )}
        {!is_demo && (
          <button
            className="primary"
            disabled={!!busy || !dirty || !canSaveDraft(draft, provider, saved, staged_login)}
          >
            {busy === 'save' ? 'Saving…' : 'Save connection'}
          </button>
        )}
      </form>

      {busy === 'test' && (
        <button type="button" className="cancel-operation" onClick={cancel}>
          Cancel test
        </button>
      )}
      {saved && (
        <div className="connection-actions">
          {!active && (
            <button
              type="button"
              disabled={!can_probe || !saved.model_id.trim()}
              onClick={() =>
                void run('activate', async (operation) => {
                  await activateConnection(saved.connection_id);
                  if (!operation.isCurrent()) return;
                  props.onActivated(saved.connection_id);
                  announce('New requests will use this connection.');
                })
              }
            >
              {busy === 'activate' ? 'Switching…' : 'Use connection'}
            </button>
          )}
          {!is_demo && (
            <>
              <button
                type="button"
                disabled={!can_probe || !saved.model_id.trim()}
                onClick={() =>
                  void run('test', async (operation) => {
                    const request_id = requestId();
                    operation.setCancellation(() => {
                      void cancelConnectionTest(request_id).catch(() => {});
                    });
                    const result = await testConnection(saved.connection_id, request_id);
                    if (!operation.isCurrent()) return;
                    announce(
                      result.status === 'completed'
                        ? `Connection works. Model: ${result.provenance.model_id ?? result.provenance.requested_model_id}.`
                        : (result.error?.message ??
                            `The test ended with status: ${result.status}.`),
                      result.status !== 'completed',
                    );
                  })
                }
              >
                {busy === 'test' ? 'Testing…' : 'Test connection'}
              </button>
              <p className="status">
                Testing sends a small model request and may use account credits.
              </p>
            </>
          )}
        </div>
      )}
      {status && (
        <p
          className={`status connection-status${failed ? ' error' : ''}`}
          role={failed ? 'alert' : 'status'}
        >
          {status}
        </p>
      )}
      {saved && saved.provider_id !== 'demo' && (
        <div className="delete-connection">
          {confirm_delete ? (
            <>
              <p>
                Remove this connection and its saved credential?
                {active ? ' New requests will use Local demo.' : ''}
              </p>
              <div className="button-row">
                <button type="button" disabled={!!busy} onClick={() => setConfirmDelete(false)}>
                  Keep connection
                </button>
                <button
                  type="button"
                  className="danger"
                  disabled={!!busy}
                  onClick={() =>
                    void run('delete', async (operation) => {
                      await deleteConnection(saved.connection_id);
                      if (operation.isCurrent()) props.onDeleted(saved.connection_id);
                    })
                  }
                >
                  Remove
                </button>
              </div>
            </>
          ) : (
            <button
              type="button"
              className="text-button danger"
              disabled={!!busy}
              onClick={() => setConfirmDelete(true)}
            >
              Remove connection
            </button>
          )}
        </div>
      )}
    </section>
  );
}
