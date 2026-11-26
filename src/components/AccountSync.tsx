import { useCallback, useEffect, useRef, useState } from 'react';
import {
  cancelAccountSignIn,
  getAccountStatus,
  getConfigSyncStatus,
  message,
  onAccountChanged,
  onConfigSyncChanged,
  onLlmSettingsChanged,
  openAccountSignIn,
  reviewConfigSync,
  setConfigSyncEnabled,
  signInToAccount,
  signOutOfAccount,
  syncConfig,
} from '../lib/tauri';
import type {
  AccountStatus,
  ConfigSyncConflict,
  ConfigSyncResolution,
  ConfigSyncStatus,
  LlmProvider,
} from '../lib/types';
import { AccountSignIn } from './AccountSignIn';
import { ConfigSyncReview } from './ConfigSyncReview';
import { EncryptedKeySync } from './EncryptedKeySync';
import { OperationScope } from './providers/state';
import { Sheet } from './Sheet';

type Action = 'sign_in' | 'open_sign_in' | 'cancel' | 'sign_out' | 'toggle' | 'sync' | 'review';

const sync_messages = {
  current: '',
  uploaded: 'Settings saved to your account.',
  downloaded: 'Synced settings applied to this device.',
};

function modelSyncLabel(
  sync: ConfigSyncStatus | undefined,
  busy: Action | undefined,
  status_error: boolean,
) {
  if (busy === 'toggle') return 'Updating…';
  if (busy === 'sync') return 'Syncing…';
  if (status_error) return 'Status unavailable';
  if (sync?.state !== 'ready') return 'Off';
  if (sync.syncing) return 'Syncing…';
  if (sync.pending_conflict_id) return 'Review needed';
  if (sync.sync_error) return 'Couldn’t sync';
  if (sync.local_changes) return 'Waiting to sync';
  return sync.last_synced_at ? 'Up to date' : 'On';
}

interface AccountSyncProps {
  providers: LlmProvider[];
  disabled: boolean;
  onApplied: () => void;
  onSyncBusyChange: (busy: boolean) => void;
  onOpenConnections: () => void;
}

export function AccountSync({
  providers,
  disabled,
  onApplied,
  onSyncBusyChange,
  onOpenConnections,
}: AccountSyncProps) {
  const [account, setAccount] = useState<AccountStatus>();
  const [sync, setSync] = useState<ConfigSyncStatus>();
  const latest_sync = useRef<ConfigSyncStatus | undefined>(undefined);
  const [busy, setBusy] = useState<Action>();
  const [keys_busy, setKeysBusy] = useState(false);
  const [conflict, setConflict] = useState<ConfigSyncConflict>();
  const [reviewing, setReviewing] = useState(false);
  const [review_stale, setReviewStale] = useState(false);
  const [outcome, setOutcome] = useState('');
  const [read_error, setReadError] = useState('');
  const [action_error, setActionError] = useState('');
  const [model_read_error, setModelReadError] = useState('');
  const [model_action_error, setModelActionError] = useState('');
  const scope = useRef(new OperationScope());
  const read_version = useRef(0);
  const visible = useRef(false);
  const session = useRef<string | null>(null);
  const account_session_id = account?.account_session_id ?? null;
  const onKeySyncBusyChange = useCallback(
    (account_session_id: string, busy: boolean) => {
      if (session.current !== account_session_id) return;
      setKeysBusy(busy);
      onSyncBusyChange(busy);
    },
    [onSyncBusyChange],
  );

  function updateSync(next?: ConfigSyncStatus) {
    latest_sync.current = next;
    setSync(next);
    if (next?.sync_error) {
      setModelActionError((previous) => (previous === next.sync_error ? '' : previous));
    }
    if (next?.state !== 'ready') clearConflict();
  }

  function acceptActionStatus(next: ConfigSyncStatus) {
    read_version.current += 1;
    updateSync(next);
    setModelReadError('');
  }

  async function refresh() {
    const version = ++read_version.current;
    let current: AccountStatus;
    try {
      current = await getAccountStatus();
    } catch (error) {
      if (read_version.current === version) setReadError(message(error));
      return;
    }
    if (read_version.current !== version) return;
    if (session.current !== current.account_session_id) {
      session.current = current.account_session_id;
      scope.current.cancel();
      setBusy(undefined);
      setKeysBusy(false);
      onSyncBusyChange(false);
      updateSync();
      setOutcome('');
      setActionError('');
      setModelReadError('');
      setModelActionError('');
    }
    setAccount(current);
    setReadError('');
    if (current.state !== 'signed_in') return;
    try {
      const synced = await getConfigSyncStatus(current.account_session_id);
      if (read_version.current !== version) return;
      updateSync(synced);
      setModelReadError('');
    } catch (error) {
      if (read_version.current === version) setModelReadError(message(error));
    }
  }

  useEffect(() => {
    let active = true;
    const listeners: (() => void)[] = [];
    visible.current = true;
    const reload = () => {
      if (active) void refresh();
    };
    reload();
    window.addEventListener('focus', reload);
    for (const subscription of [
      onAccountChanged(reload),
      onConfigSyncChanged(reload),
      onLlmSettingsChanged(reload),
    ]) {
      void subscription.then(
        (unlisten) => {
          if (!active) unlisten();
          else {
            listeners.push(unlisten);
            reload();
          }
        },
        () => {},
      );
    }
    return () => {
      active = false;
      visible.current = false;
      window.removeEventListener('focus', reload);
      for (const unlisten of listeners) unlisten();
      read_version.current += 1;
      // Activity can hide this panel while a native sync is still in progress.
      // Let that operation finish and retain its outcome for the next visit.
    };
  }, [onSyncBusyChange]);

  function clearConflict() {
    setConflict(undefined);
    setReviewing(false);
    setReviewStale(false);
  }

  async function perform(action: Action, run: (isCurrent: () => boolean) => Promise<void>) {
    if (keys_busy || session.current !== account_session_id) return;
    const operation = scope.current.start();
    if (!operation) return;
    read_version.current += 1;
    setBusy(action);
    setOutcome('');
    const sync_operation = action === 'sync' || action === 'toggle' || action === 'review';
    if (sync_operation) setModelActionError('');
    else setActionError('');
    const isCurrent = () => operation.isCurrent() && account_session_id === session.current;
    if (sync_operation) onSyncBusyChange(true);
    try {
      await run(isCurrent);
    } catch (error) {
      if (isCurrent()) {
        const detail = message(error);
        if (sync_operation) {
          if (detail !== latest_sync.current?.sync_error) setModelActionError(detail);
        } else setActionError(detail);
        if (sync_operation || action === 'sign_out') void refresh();
      }
    } finally {
      if (isCurrent()) {
        if (sync_operation) onSyncBusyChange(false);
        operation.finish();
        setBusy(undefined);
      }
    }
  }

  function changeAccount(
    action: 'sign_in' | 'cancel' | 'sign_out',
    request: () => Promise<AccountStatus>,
  ) {
    return perform(action, async (isCurrent) => {
      await request();
      if (isCurrent()) await refresh();
    });
  }

  function toggleSync(enabled: boolean) {
    if (!account_session_id) return;
    return perform('toggle', async (isCurrent) => {
      const next = await setConfigSyncEnabled(account_session_id, enabled);
      if (!isCurrent()) return;
      acceptActionStatus(next);
      if (enabled) void refresh();
    });
  }

  function runSync(resolution?: ConfigSyncResolution) {
    if (!account_session_id) return;
    if (resolution && latest_sync.current?.pending_conflict_id !== resolution.conflict_id) {
      setReviewStale(true);
      return;
    }
    return perform('sync', async (isCurrent) => {
      try {
        await syncSettings(account_session_id, isCurrent, resolution);
      } catch (error) {
        if (!isCurrent()) return;
        if (!resolution) {
          // The native status can already include a newer background attempt.
          await refresh();
          return;
        }
        setReviewStale(true);
        throw error;
      }
    });
  }

  function refreshReview() {
    if (!account_session_id) return;
    return perform('review', async (isCurrent) => {
      const result = await reviewConfigSync(account_session_id);
      if (!isCurrent()) return;
      if (result.state === 'conflict')
        await showFreshConflict(account_session_id, result, isCurrent);
      else {
        acceptActionStatus(result.status);
        clearConflict();
        setOutcome('No changes need review.');
        void refresh();
      }
    });
  }

  async function showFreshConflict(
    account_session_id: string,
    result: ConfigSyncConflict,
    isCurrent: () => boolean,
  ) {
    // Check local status again before showing the comparison. A background pass
    // or a local edit may have replaced the conflict while the review loaded.
    const before = latest_sync.current;
    let fresh: ConfigSyncStatus;
    try {
      fresh = await getConfigSyncStatus(account_session_id);
    } catch (error) {
      if (isCurrent()) setReviewStale(true);
      throw error;
    }
    if (!isCurrent()) return;
    const observed = latest_sync.current;
    const superseded =
      (observed?.state !== before?.state ||
        observed?.pending_conflict_id !== before?.pending_conflict_id) &&
      (observed?.state !== fresh.state ||
        observed?.pending_conflict_id !== fresh.pending_conflict_id);
    if (!superseded) acceptActionStatus(fresh);
    if (superseded || fresh.state !== 'ready' || fresh.pending_conflict_id !== result.conflict_id) {
      setReviewStale(true);
      setModelActionError(
        'Model settings changed. Check the latest changes before choosing a copy.',
      );
      void refresh();
      return;
    }
    setConflict(result);
    setReviewStale(false);
    setReviewing(visible.current);
  }

  async function syncSettings(
    account_session_id: string,
    isCurrent: () => boolean,
    resolution?: ConfigSyncResolution,
  ) {
    const result = await syncConfig(account_session_id, resolution);
    if (!isCurrent()) return;
    // The native save can finish while Settings is hidden. Refresh the
    // connection list independently of the panel's transient feedback.
    if (result.state === 'downloaded') onApplied();
    if (result.state === 'conflict') {
      await showFreshConflict(account_session_id, result, isCurrent);
      return;
    }
    acceptActionStatus(result.status);
    clearConflict();
    setOutcome(sync_messages[result.state]);
    void refresh();
  }

  const last_sync = sync?.last_synced_at ? new Date(sync.last_synced_at) : null;
  const last_sync_label =
    last_sync && Number.isFinite(last_sync.getTime()) ? last_sync.toLocaleString() : undefined;
  const error = action_error || read_error || account?.error;
  const model_status_unavailable = !!(read_error || model_read_error);
  const ready =
    account?.state === 'signed_in' && !model_status_unavailable && sync?.state === 'ready';
  const pending_conflict = ready && !!sync.pending_conflict_id;
  const review_conflict = account?.state === 'signed_in' && conflict;
  const review_open = !!review_conflict && reviewing;
  const review_outdated =
    review_stale || model_status_unavailable || sync?.pending_conflict_id !== conflict?.conflict_id;
  const syncing = busy === 'sync' || (ready && sync.syncing);
  const model_error =
    model_read_error || model_action_error || (ready && !syncing ? sync.sync_error : '');

  return (
    <section
      className="panel account-sync"
      aria-labelledby="settings-account"
      aria-busy={!!busy || keys_busy || (!account && !read_error)}
    >
      <h2 id="settings-account" className="settings-section-title" tabIndex={-1}>
        Apell Account
      </h2>
      {!account && !read_error && <p role="status">Checking sign-in…</p>}
      {account?.state === 'unavailable' && <p>Account sign-in is unavailable in this build.</p>}
      {account?.state === 'signed_out' && (
        <>
          <p>Sign in to sync model settings and set up encrypted API-key sync.</p>
          <button
            type="button"
            className="primary"
            disabled={!!busy}
            onClick={() => void changeAccount('sign_in', signInToAccount)}
          >
            {busy === 'sign_in' ? 'Requesting a code…' : 'Sign in'}
          </button>
        </>
      )}
      {account?.state === 'signing_in' && (
        <AccountSignIn
          challenge={account.device_code}
          busy={!!busy}
          error={error}
          onOpen={() => void perform('open_sign_in', openAccountSignIn)}
          onCancel={() => void changeAccount('cancel', cancelAccountSignIn)}
        />
      )}
      {account?.state === 'signed_in' && (
        <>
          <p className="account-sync-identity">{account.email ?? 'Signed in'}</p>
          <section className="account-model-sync" aria-labelledby="account-model-settings-title">
            <h3 id="account-model-settings-title">Model settings</h3>
            {!sync && !model_status_unavailable && <p role="status">Checking sync…</p>}
            {model_status_unavailable && (!sync || sync.state === 'disabled') && (
              <p role="status">Status unavailable</p>
            )}
            {!model_status_unavailable && sync?.state === 'disabled' && (
              <p>Settings sync is unavailable in this build.</p>
            )}
            {sync && sync.state !== 'disabled' && (
              <>
                <div className="account-model-sync-row">
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={sync.state === 'ready'}
                      disabled={disabled || !!busy || keys_busy || model_status_unavailable}
                      onChange={(event) => void toggleSync(event.target.checked)}
                    />
                    Sync model settings
                  </label>
                  <span className="account-model-sync-state" role="status">
                    {modelSyncLabel(sync, busy, model_status_unavailable)}
                  </span>
                </div>
                <p>
                  Automatically sync built-in provider connections and model choices. Verified
                  devices use these connections to restore their API keys.
                </p>
              </>
            )}
            {model_error && !review_open && (
              <p className="account-model-sync-error" role="alert">
                {model_error}
              </p>
            )}
            {ready && (
              <>
                {sync.local_changes && !pending_conflict && !syncing && !model_error && (
                  <p role="status">Changes on this device haven’t synced yet.</p>
                )}
                <div className="account-sync-actions">
                  {pending_conflict && (
                    <button
                      type="button"
                      className="primary"
                      disabled={disabled || !!busy || keys_busy}
                      onClick={() => void refreshReview()}
                    >
                      {busy === 'review' ? 'Checking changes…' : 'Review changes'}
                    </button>
                  )}
                  <button
                    type="button"
                    disabled={disabled || !!busy || keys_busy || syncing}
                    onClick={() => void runSync()}
                  >
                    Sync now
                  </button>
                </div>
                {last_sync_label && (
                  <p>
                    Cloud updated{' '}
                    <time dateTime={sync.last_synced_at ?? undefined}>{last_sync_label}</time>
                  </p>
                )}
              </>
            )}
            {model_read_error && !read_error && !busy && !review_open && (
              <button type="button" onClick={() => void refresh()}>
                Check again
              </button>
            )}
            {outcome &&
              !model_status_unavailable &&
              !pending_conflict &&
              !syncing &&
              !model_error && (
                <p className="status" role="status">
                  {outcome}
                </p>
              )}
          </section>
          <EncryptedKeySync
            key={account.account_session_id}
            account_session_id={account.account_session_id}
            providers={providers}
            model_review_needed={pending_conflict}
            model_sync_enabled={
              model_status_unavailable || sync?.state === 'disabled'
                ? undefined
                : sync && sync.state === 'ready'
            }
            disabled={disabled || !!busy || review_open}
            onApplied={() => {
              if (session.current === account.account_session_id) onApplied();
            }}
            onBusyChange={(busy) => onKeySyncBusyChange(account.account_session_id, busy)}
            onOpenConnections={onOpenConnections}
          />
          <button
            className="account-sign-out"
            type="button"
            disabled={!!busy || keys_busy}
            onClick={() =>
              void changeAccount('sign_out', () => signOutOfAccount(account.account_session_id))
            }
          >
            {busy === 'sign_out' ? 'Signing out…' : 'Sign out'}
          </button>
        </>
      )}
      {error && !review_open && account?.state !== 'signing_in' && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
      {read_error && !busy && (
        <button type="button" onClick={() => void refresh()}>
          Check again
        </button>
      )}
      {review_conflict && review_open && (
        <Sheet
          open
          title="Choose which model settings to use"
          onClose={() => {
            clearConflict();
            setModelActionError('');
          }}
          dismissible={!busy}
        >
          <ConfigSyncReview
            conflict={review_conflict}
            providers={providers}
            busy={!!busy}
            disabled={disabled || keys_busy}
            stale={review_outdated}
            error={
              model_read_error ||
              model_action_error ||
              error ||
              (review_outdated
                ? sync?.sync_error || 'Model settings changed. Refresh the review.'
                : '')
            }
            onRefresh={() => void refreshReview()}
            onKeepLocal={() =>
              void runSync({ resolution: 'keep_local', conflict_id: review_conflict.conflict_id })
            }
            onUseCloud={() =>
              void runSync({ resolution: 'use_cloud', conflict_id: review_conflict.conflict_id })
            }
          />
        </Sheet>
      )}
    </section>
  );
}
