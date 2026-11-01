import { useEffect, useRef, useState } from 'react';
import {
  activateConnection,
  beginLogin,
  cancelConnectionTest,
  cancelLogin,
  deleteConnection,
  discoverModels,
  getSettings,
  message,
  openLogin,
  pollLogin,
  saveConnection,
  testConnection,
} from '../../lib/tauri';
import type { LlmConnection, LlmModels, LlmProvider, LoginChallenge } from '../../lib/types';
import { Icon } from '../Icon';
import { ModelPicker } from './ModelPicker';
import {
  changeProvider,
  connectionDraft,
  destinationChanged,
  hasChanges,
  OperationScope,
  requestId,
  savePayload,
  stopLogin,
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
  const [busy, setBusy] = useState('');
  const [status, setStatus] = useState('');
  const [failed, setFailed] = useState(false);
  const [confirm_delete, setConfirmDelete] = useState(false);
  const scope = useRef(new OperationScope());
  const login_operation = useRef<Operation | undefined>(undefined);
  const login_begin = useRef<Promise<LoginChallenge> | undefined>(undefined);
  const provider = props.providers.find((item) => item.provider_id === draft.provider_id);
  const is_demo = draft.provider_id === 'demo';
  const device_login = provider?.auth_methods.includes('device_code') ?? false;
  const accepts_key = provider?.auth_methods.includes('api_key') ?? false;
  const changed_destination = destinationChanged(draft, saved);
  const dirty = hasChanges(draft, saved);
  const can_probe = !!saved && !dirty && !busy;
  const active = saved?.connection_id === props.active_connection_id;

  useEffect(() => () => scope.current.cancel(), []);

  const announce = (text: string, error = false) => {
    setStatus(text);
    setFailed(error);
  };
  const acceptSaved = (connection: LlmConnection) => {
    props.onSaved(connection, saved?.connection_id);
    setSaved(connection);
    setDraft(connectionDraft(connection, props.providers));
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
        login_begin.current = undefined;
      }
    }
  };
  const cancel = () => {
    const was_login = busy === 'login';
    const pending_begin = login_begin.current;
    scope.current.cancel();
    login_operation.current = undefined;
    setBusy('');
    setLogin(undefined);
    if (!was_login) {
      announce('Test cancelled.');
      return;
    }
    void run('cancel_login', async (operation) => {
      // A finished sign-in may win the cancellation race. Wait for the native
      // acknowledgement, then read its authoritative credential status.
      const settings = await stopLogin(pending_begin, cancelLogin, getSettings);
      if (!operation.isCurrent()) return;
      const connection = settings.connections.find(
        (item) => item.connection_id === saved?.connection_id,
      );
      if (connection) acceptSaved(connection);
      announce('Sign-in stopped. Saved credential status refreshed.');
    });
  };
  const signIn = () =>
    void run('login', async (operation) => {
      const pending = beginLogin(saved!.connection_id);
      login_begin.current = pending;
      const challenge = await pending;
      operation.setCancellation(() => {
        void cancelLogin(challenge.login_id).catch(() => {});
      });
      if (!operation.isCurrent()) return;
      login_operation.current = operation;
      setLogin(challenge);
      const connection = await pollLogin(challenge.login_id);
      if (!operation.isCurrent()) return;
      acceptSaved(connection);
      announce('Signed in. Refresh models to choose a model.');
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
          : 'Save a connection, then choose when to use it.'}
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void run('save', async (operation) => {
            const connection = await saveConnection(savePayload(draft, saved));
            if (!operation.isCurrent()) return;
            acceptSaved(connection);
            if (changed_destination) setModels(undefined);
            announce('Connection saved.');
          });
        }}
      >
        <fieldset disabled={!!busy}>
          <label>
            Name
            <input
              required
              maxLength={80}
              disabled={is_demo}
              value={draft.name}
              autoComplete="off"
              onChange={(event) => setDraft({ ...draft, name: event.target.value })}
            />
          </label>
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
                setDraft({
                  ...changeProvider(draft, next),
                  name: draft.name === provider?.name ? next.name : draft.name,
                });
                setModels(undefined);
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
              {accepts_key && (
                <div className="credential-fields">
                  <label>
                    API key
                    <select
                      value={draft.key_action}
                      onChange={(event) =>
                        setDraft({
                          ...draft,
                          key_action: event.target.value as typeof draft.key_action,
                          api_key: '',
                        })
                      }
                    >
                      <option value="keep">
                        {saved && !changed_destination ? 'Keep current credential' : 'No new key'}
                      </option>
                      <option value="replace">Enter a new key</option>
                      <option value="remove">Remove saved key</option>
                    </select>
                  </label>
                  {draft.key_action === 'replace' && (
                    <label>
                      New API key
                      <input
                        type="password"
                        required
                        maxLength={8192}
                        value={draft.api_key}
                        autoCapitalize="none"
                        autoCorrect="off"
                        autoComplete="off"
                        spellCheck={false}
                        onChange={(event) => setDraft({ ...draft, api_key: event.target.value })}
                      />
                    </label>
                  )}
                  {saved && !changed_destination && (
                    <p
                      className={`status${saved.credential_status === 'unavailable' ? ' error' : ''}`}
                    >
                      {saved.credential_status === 'present'
                        ? 'Credential saved securely.'
                        : saved.credential_status === 'unavailable'
                          ? 'The secure credential store is unavailable. You can still edit this connection.'
                          : 'No API key saved.'}
                    </p>
                  )}
                </div>
              )}
              {device_login && (
                <div className="credential-fields">
                  <p className="status">
                    {saved?.credential_status === 'present' && !changed_destination
                      ? 'Signed in securely.'
                      : saved?.credential_status === 'unavailable'
                        ? 'The secure credential store is unavailable.'
                        : 'Sign in with your account using a one-time code.'}
                  </p>
                  <button type="button" disabled={!can_probe} onClick={signIn}>
                    {saved?.credential_status === 'present' ? 'Sign in again' : 'Sign in'}
                  </button>
                  {saved?.credential_status === 'present' && !changed_destination && (
                    <button
                      type="button"
                      className="text-button danger"
                      onClick={() => {
                        setDraft({ ...draft, key_action: 'remove', api_key: '' });
                        announce('Save to remove the saved sign-in.');
                      }}
                    >
                      Remove sign-in
                    </button>
                  )}
                </div>
              )}
              <ModelPicker
                model_id={draft.model_id}
                models={models}
                disabled={!!busy}
                onChange={(model_id) => setDraft({ ...draft, model_id })}
              />
              <button
                type="button"
                disabled={!can_probe}
                onClick={() =>
                  void run('models', async (operation) => {
                    const result = await discoverModels(saved!.connection_id);
                    if (!operation.isCurrent()) return;
                    setModels(result);
                    announce(
                      result.models.length
                        ? 'Choose a model below or enter its ID.'
                        : 'No model list is available. Enter a model ID manually.',
                    );
                  })
                }
              >
                {busy === 'models' ? 'Refreshing models…' : 'Refresh models'}
              </button>
              {device_login ? (
                <p className="status">The sign-in provider manages its API endpoint.</p>
              ) : (
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
                        setDraft({ ...draft, base_url: event.target.value });
                        setModels(undefined);
                      }}
                    />
                  </label>
                </details>
              )}
              {changed_destination && (
                <p className="status">
                  This is a different destination. The previous credential will not be reused.
                </p>
              )}
            </>
          )}
        </fieldset>
        {dirty && !is_demo && (
          <p className="status">
            Save changes before signing in, refreshing models, or testing. You can select a model
            later.
          </p>
        )}
        {!is_demo && (
          <button className="primary" disabled={!!busy || !dirty || !provider}>
            {busy === 'save' ? 'Saving…' : 'Save connection'}
          </button>
        )}
      </form>

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
      {(busy === 'login' || busy === 'test') && (
        <button type="button" className="cancel-operation" onClick={cancel}>
          Cancel {busy === 'login' ? 'sign-in' : 'test'}
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
