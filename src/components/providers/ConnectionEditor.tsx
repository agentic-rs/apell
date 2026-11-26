import { useEffect, useLayoutEffect, useRef, useState } from 'react';
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
  saved_connection_changed,
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
import { Sheet } from '../Sheet';
import { useBackAction } from '../../lib/backNavigation';
import { connectionReadiness, connectionReadinessLabel } from '../../lib/modelChoice';
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
  hasUnsavedInputs,
  OperationScope,
  requestId,
  savePayload,
  type Operation,
} from './state';

interface EditorProps {
  connection?: LlmConnection;
  providers: LlmProvider[];
  mock_credentials: boolean;
  active_connection_id: string | null;
  back_label?: string;
  onSaved: (connection: LlmConnection, previous_id?: string) => void;
  onActivated: (connection_id: string) => void;
  onDeleted: (connection_id: string) => void;
  onBack: (refresh?: boolean) => void;
}

const expired_draft =
  'This connection draft expired or was discarded. Reopen the editor to continue.';
export function ConnectionEditor(props: EditorProps) {
  const [saved, setSaved] = useState(props.connection);
  const [draft, setDraft] = useState(() => connectionDraft(props.connection, props.providers));
  const original_draft = useRef(draft);
  const [models, setModels] = useState<LlmModels>();
  const [login, setLogin] = useState<LoginChallenge>();
  const [native_draft, setNativeDraft] = useState<NativeConnectionDraft>();
  const [staged_login, setStagedLogin] = useState(false);
  const [sign_in_expired, setSignInExpired] = useState(false);
  const [stale_review, setStaleReview] = useState<{ staged_sign_in: boolean }>();
  const source_changed = stale_review !== undefined;
  const [busy, setBusy] = useState('');
  const back_disabled = ['save', 'activate', 'delete'].includes(busy);
  const [status, setStatus] = useState<{ text: string; failed: boolean; setup: boolean }>();
  const [confirm_delete, setConfirmDelete] = useState(false);
  const [confirm_discard, setConfirmDiscard] = useState(false);
  const scope = useRef(new OperationScope());
  const drafts = useRef(new DraftSession(createConnectionDraft, discardConnectionDraft));
  const login_operation = useRef<Operation | undefined>(undefined);
  const stale_notice = useRef<HTMLParagraphElement>(null);
  const leave = () => {
    scope.current.cancel();
    void drafts.current.discard().catch(() => {});
    props.onBack(source_changed);
  };
  const goBack = () => {
    if (back_disabled) return;
    if (confirm_delete) setConfirmDelete(false);
    else if (
      busy === 'login' ||
      sign_in_expired ||
      stale_review?.staged_sign_in ||
      hasUnsavedInputs(draft, saved ?? original_draft.current, staged_login)
    )
      setConfirmDiscard(true);
    else leave();
  };
  useBackAction(25, true, goBack);
  const provider = props.providers.find((item) => item.provider_id === draft.provider_id);
  const device_login = provider?.auth_methods.includes('device_code') ?? false;
  const sign_in_copy = props.mock_credentials
    ? {
        start: 'Use a test account. Apell Local saves sign-ins in unencrypted files.',
        staged: 'Signed in for this setup. Saving keeps this sign-in in an unencrypted file.',
        saved: 'Sign-in saved in an unencrypted test file.',
        unavailable: 'The local credential store is unavailable.',
      }
    : {
        start: 'Sign in with your account using a one-time code.',
        staged: 'Signed in for this setup. Save to keep this account.',
        saved: 'Signed in securely.',
        unavailable: 'The secure credential store is unavailable.',
      };
  const accepts_key = provider?.auth_methods.includes('api_key') ?? false;
  const changed_destination = destinationChanged(draft, saved);
  const dirty = hasChanges(draft, saved, staged_login);
  const readiness = saved && connectionReadiness(saved, props.providers);
  const readiness_label = readiness && connectionReadinessLabel(readiness);
  const can_probe =
    readiness?.state === 'ready' && !dirty && !busy && !sign_in_expired && !source_changed;
  const active = saved?.connection_id === props.active_connection_id;
  const inherited_status = !changed_destination ? saved?.credential_status : undefined;
  const credential_status = native_draft?.credential_status ?? inherited_status;
  const can_discover =
    !busy &&
    !source_changed &&
    !sign_in_expired &&
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
  useLayoutEffect(() => {
    if (source_changed) stale_notice.current?.focus();
  }, [source_changed]);

  const announce = (text: string, failed = false, setup = false) =>
    setStatus(text ? { text, failed, setup } : undefined);
  const discardDraft = () => {
    const disposal = drafts.current.discard();
    setNativeDraft(undefined);
    setStagedLogin(false);
    setModels(undefined);
    setLogin(undefined);
    return disposal;
  };
  const handleDraftFailure = (error: unknown, was_signed_in = staged_login) => {
    const detail = message(error);
    if (detail === saved_connection_changed) {
      void discardDraft().catch(() => {});
      setStaleReview({ staged_sign_in: was_signed_in });
      return true;
    }
    if (detail !== expired_draft) return false;
    void discardDraft().catch(() => {});
    const lost_sign_in = (was_signed_in || sign_in_expired) && !draft.remove_credential;
    setSignInExpired(lost_sign_in);
    announce(
      lost_sign_in
        ? 'Your temporary sign-in expired. Sign in again before saving it.'
        : 'This setup expired. Your changes are still here. Try again.',
      true,
      true,
    );
    return true;
  };
  const acceptSaved = (connection: LlmConnection) => {
    props.onSaved(connection, saved?.connection_id);
    setSaved(connection);
    setDraft(connectionDraft(connection, props.providers));
    setNativeDraft(undefined);
    setStagedLogin(false);
    setSignInExpired(false);
  };
  const run = async (kind: string, action: (operation: Operation) => Promise<void>) => {
    if (source_changed) return;
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(kind);
    setConfirmDelete(false);
    announce('');
    try {
      await action(operation);
    } catch (error) {
      if (operation.isCurrent() && !handleDraftFailure(error)) announce(message(error), true);
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
      await discardDraft();
      if (!operation.isCurrent()) return;
      announce('Sign-in cancelled. No credential was saved.');
    });
  };
  const resetDestination = () => {
    scope.current.cancel();
    void discardDraft().catch(() => {});
    setSignInExpired(false);
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
      setSignInExpired(false);
      setDraft((current) => ({ ...current, remove_credential: false, api_key: '' }));
      setLogin(undefined);
      setBusy('models');
      try {
        await loadModels(operation, authenticated, true);
      } catch (error) {
        if (operation.isCurrent() && !handleDraftFailure(error, true))
          announce(`Signed in for this setup. ${message(error)}`, true);
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
  const feedback = status && (
    <p
      className={`status connection-status${status.failed ? ' error' : ''}`}
      role={status.failed ? 'alert' : 'status'}
    >
      {status.text}
    </p>
  );

  return (
    <section className="settings connection-editor panel" aria-busy={!!busy}>
      <div className="settings-heading">
        <button type="button" className="text-button" disabled={back_disabled} onClick={goBack}>
          <Icon name="back" /> {props.back_label ?? 'Connections'}
        </button>
        {active && (
          <span className="connection-badge">
            {readiness?.state === 'ready' ? 'Default' : 'Selected'}
          </span>
        )}
      </div>
      <h2 id="settings-connection-editor" className="settings-section-title" tabIndex={-1}>
        {saved ? saved.name : 'Add connection'}
      </h2>
      <p>
        {readiness_label
          ? `${readiness_label}.`
          : active
            ? 'Applets use this connection by default.'
            : 'Choose a provider, connect your account, and select a model.'}
      </p>
      {source_changed && (
        <div className="notice connection-review-needed">
          <p ref={stale_notice} tabIndex={-1} role="alert">
            {saved_connection_changed}
          </p>
          {stale_review.staged_sign_in && <p>The temporary sign-in for this draft ended.</p>}
          <button type="button" onClick={goBack}>
            {props.back_label ? `Back to ${props.back_label}` : 'Review saved connections'}
          </button>
        </div>
      )}
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (source_changed || sign_in_expired) return;
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
        <fieldset disabled={!!busy || source_changed}>
          <label>
            Provider
            <select
              value={draft.provider_id}
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
              {props.providers.map((item) => (
                <option key={item.provider_id} value={item.provider_id}>
                  {item.name}
                </option>
              ))}
              {!provider && (
                <option value={draft.provider_id}>{draft.provider_id} (unavailable)</option>
              )}
            </select>
          </label>
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
                    if (draftPayload(next, saved).base_url !== draftPayload(draft, saved).base_url)
                      resetDestination();
                    setDraft(next);
                  }}
                />
              </label>
            </details>
          )}
          {saved && changed_destination && (
            <p className="status">
              Saving replaces this connection and any saved credentials on this device.
            </p>
          )}
          {draft.provider_id === 'zai-coding-plan' && (
            <p className="status">
              If you’re creating a new key, use a Personal or Team Coding Plan key from Z.ai.
            </p>
          )}
          {accepts_key && (
            <ApiKeyField
              credential_status={credential_status}
              mock_credentials={props.mock_credentials}
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
                    : sign_in_copy.staged
                  : credential_status === 'present'
                    ? sign_in_copy.saved
                    : credential_status === 'unavailable'
                      ? sign_in_copy.unavailable
                      : sign_in_copy.start}
              </p>
              <button type="button" disabled={!!busy} onClick={signIn}>
                {sign_in_expired || credential_status === 'present' ? 'Sign in again' : 'Sign in'}
              </button>
              {sign_in_expired && inherited_status === 'present' && (
                <button
                  type="button"
                  onClick={() => {
                    setSignInExpired(false);
                    announce('Your saved sign-in will be kept.');
                  }}
                >
                  Keep saved sign-in
                </button>
              )}
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
                    if (!draft.remove_credential) setSignInExpired(false);
                    announce(draft.remove_credential ? '' : 'Save to remove the sign-in.');
                  }}
                >
                  {draft.remove_credential ? 'Undo removal' : 'Remove sign-in'}
                </button>
              )}
            </div>
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
        <fieldset disabled={!!busy || source_changed}>
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
        </fieldset>
        <button
          className="primary"
          disabled={
            !!busy ||
            source_changed ||
            sign_in_expired ||
            !dirty ||
            !canSaveDraft(draft, provider, saved, staged_login)
          }
        >
          {busy === 'save' ? 'Saving…' : 'Save connection'}
        </button>
        {status?.setup && feedback}
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
              disabled={!can_probe}
              onClick={() =>
                void run('activate', async (operation) => {
                  await activateConnection(saved);
                  if (!operation.isCurrent()) return;
                  props.onActivated(saved.connection_id);
                  announce('Default model updated.');
                })
              }
            >
              {busy === 'activate' ? 'Switching…' : 'Set as default'}
            </button>
          )}
          <button
            type="button"
            disabled={!can_probe}
            onClick={() =>
              void run('test', async (operation) => {
                const request_id = requestId();
                operation.setCancellation(() => {
                  void cancelConnectionTest(request_id).catch(() => {});
                });
                const result = await testConnection(saved, request_id);
                if (!operation.isCurrent()) return;
                announce(
                  result.status === 'completed'
                    ? `Connection works. Model: ${result.provenance.model_id ?? result.provenance.requested_model_id}.`
                    : (result.error?.message ?? `The test ended with status: ${result.status}.`),
                  result.status !== 'completed',
                );
              })
            }
          >
            {busy === 'test' ? 'Testing…' : 'Test connection'}
          </button>
          {readiness?.state === 'ready' && (
            <p className="status">
              Testing sends a small model request and may use account credits.
            </p>
          )}
        </div>
      )}
      {!status?.setup && feedback}
      {saved && (
        <div className="delete-connection">
          {confirm_delete ? (
            <>
              <p>
                Remove this connection from this device and delete its saved credential?
                {active ? ' Choose another connection to set a new default model.' : ''}
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
                      await deleteConnection(saved);
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
              disabled={!!busy || source_changed}
              onClick={() => setConfirmDelete(true)}
            >
              Remove connection
            </button>
          )}
        </div>
      )}
      <Sheet
        open={confirm_discard}
        title="Discard connection changes?"
        onClose={() => setConfirmDiscard(false)}
      >
        <p>Your changes to this connection setup will be lost.</p>
        <div className="button-row">
          <button type="button" onClick={() => setConfirmDiscard(false)}>
            {source_changed ? 'Keep draft' : 'Keep editing'}
          </button>
          <button type="button" className="danger" onClick={leave}>
            Discard changes
          </button>
        </div>
      </Sheet>
    </section>
  );
}
