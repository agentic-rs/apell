import { useCallback, useEffect, useRef, useState } from 'react';
import {
  cancelAccountSignIn,
  getAccountStatus,
  getConfigSyncStatus,
  message,
  onAccountChanged,
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
  current: 'Settings are up to date.',
  uploaded: 'Settings saved to your account.',
  downloaded: 'Synced settings applied to this device.',
};

interface AccountSyncProps {
  providers: LlmProvider[];
  disabled: boolean;
  onApplied: () => void;
  onSyncBusyChange: (busy: boolean) => void;
}

export function AccountSync({
  providers,
  disabled,
  onApplied,
  onSyncBusyChange,
}: AccountSyncProps) {
  const [account, setAccount] = useState<AccountStatus>();
  const [sync, setSync] = useState<ConfigSyncStatus>();
  const [busy, setBusy] = useState<Action>();
  const [keys_busy, setKeysBusy] = useState(false);
  const [conflict, setConflict] = useState<ConfigSyncConflict>();
  const [reviewing, setReviewing] = useState(false);
  const [review_stale, setReviewStale] = useState(false);
  const [outcome, setOutcome] = useState('');
  const [read_error, setReadError] = useState('');
  const [action_error, setActionError] = useState('');
  const scope = useRef(new OperationScope());
  const read_version = useRef(0);
  const visible = useRef(false);
  const sync_account = useRef<{ email: string | null } | undefined>(undefined);
  const onKeySyncBusyChange = useCallback(
    (busy: boolean) => {
      setKeysBusy(busy);
      onSyncBusyChange(busy);
    },
    [onSyncBusyChange],
  );

  async function refresh() {
    const version = ++read_version.current;
    try {
      const current = await getAccountStatus();
      if (read_version.current !== version) return;
      const email = current.state === 'signed_in' ? current.email : undefined;
      if (sync_account.current?.email !== email) {
        sync_account.current = email === undefined ? undefined : { email };
        setSync(undefined);
        clearConflict();
        setOutcome('');
        setActionError('');
      }
      setAccount(current);
      if (current.state === 'signed_in') {
        const synced = await getConfigSyncStatus();
        if (read_version.current !== version) return;
        setSync(synced);
        if (synced.state !== 'ready') clearConflict();
      }
      setReadError('');
    } catch (error) {
      if (read_version.current === version) setReadError(message(error));
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
    for (const subscription of [onAccountChanged(reload), onLlmSettingsChanged(reload)]) {
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
    if (keys_busy) return;
    const operation = scope.current.start();
    if (!operation) return;
    read_version.current += 1;
    setBusy(action);
    setActionError('');
    setOutcome('');
    const sync_operation = action === 'sync' || action === 'toggle' || action === 'review';
    const started_account = sync_account.current;
    const isCurrent = () =>
      operation.isCurrent() && (!sync_operation || started_account === sync_account.current);
    if (sync_operation) onSyncBusyChange(true);
    try {
      await run(isCurrent);
    } catch (error) {
      if (isCurrent()) setActionError(message(error));
    } finally {
      if (sync_operation) onSyncBusyChange(false);
      if (operation.isCurrent()) {
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
    return perform('toggle', async (isCurrent) => {
      const next = await setConfigSyncEnabled(enabled);
      if (!isCurrent()) return;
      setSync(next);
      if (next.state !== 'ready') clearConflict();
      if (enabled && next.state === 'ready' && !next.last_synced_at) {
        setBusy('sync');
        await syncSettings(isCurrent);
      }
    });
  }

  function runSync(resolution?: ConfigSyncResolution) {
    return perform('sync', async (isCurrent) => {
      try {
        await syncSettings(isCurrent, resolution);
      } catch (error) {
        if (resolution && isCurrent()) setReviewStale(true);
        throw error;
      }
    });
  }

  function refreshReview() {
    return perform('review', async (isCurrent) => {
      const result = await reviewConfigSync();
      if (!isCurrent()) return;
      setSync(result.status);
      if (result.state === 'conflict') showConflict(result);
      else {
        clearConflict();
        setOutcome('No differences to review. Use Sync now to check for changes.');
      }
    });
  }

  function showConflict(result: ConfigSyncConflict) {
    setConflict(result);
    setReviewStale(false);
    setReviewing(visible.current);
  }

  async function syncSettings(isCurrent: () => boolean, resolution?: ConfigSyncResolution) {
    const result = await syncConfig(resolution);
    // The native save can finish while Settings is hidden. Refresh the
    // connection list independently of the panel's transient feedback.
    if (result.state === 'downloaded') onApplied();
    if (!isCurrent()) return;
    setSync(result.status);
    if (result.state === 'conflict') {
      showConflict(result);
      return;
    }
    clearConflict();
    setOutcome(sync_messages[result.state]);
  }

  const last_sync = sync?.last_synced_at ? new Date(sync.last_synced_at) : null;
  const last_sync_label =
    last_sync && Number.isFinite(last_sync.getTime()) ? last_sync.toLocaleString() : undefined;
  const error = action_error || read_error || account?.error;
  const pending_conflict = account?.state === 'signed_in' && sync?.state === 'ready' && conflict;
  const review_open = !!pending_conflict && reviewing;

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
            {!sync && !read_error && <p role="status">Checking sync…</p>}
            {sync?.state === 'disabled' && <p>Settings sync is unavailable in this build.</p>}
            {sync && sync.state !== 'disabled' && (
              <>
                <div className="account-model-sync-row">
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={sync.state === 'ready'}
                      disabled={disabled || !!busy || keys_busy}
                      onChange={(event) => void toggleSync(event.target.checked)}
                    />
                    Sync model settings
                  </label>
                  <span className="account-model-sync-state" role="status">
                    {busy === 'sync'
                      ? 'Syncing…'
                      : busy === 'toggle'
                        ? 'Updating…'
                        : sync.state === 'ready'
                          ? 'On'
                          : 'Off'}
                  </span>
                </div>
                <p>
                  Sync built-in provider connections and model choices. Verified devices use these
                  connections to restore their API keys.
                </p>
              </>
            )}
            {sync?.state === 'ready' && (
              <>
                {pending_conflict && <p role="status">Model settings need review.</p>}
                {sync.local_changes && !pending_conflict && !busy && (
                  <p role="status">Changes on this device haven’t synced yet.</p>
                )}
                <div className="account-sync-actions">
                  {pending_conflict && (
                    <button
                      type="button"
                      className="primary"
                      disabled={disabled || !!busy || keys_busy}
                      onClick={() => setReviewing(true)}
                    >
                      Review changes
                    </button>
                  )}
                  <button
                    type="button"
                    disabled={disabled || !!busy || keys_busy}
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
            {outcome && (
              <p className="status" role="status">
                {outcome}
              </p>
            )}
          </section>
          <EncryptedKeySync
            model_sync_enabled={
              sync?.state === 'disabled' ? undefined : sync && sync.state === 'ready'
            }
            disabled={disabled || !!busy || review_open}
            onApplied={onApplied}
            onBusyChange={onKeySyncBusyChange}
          />
          <button
            className="account-sign-out"
            type="button"
            disabled={!!busy || keys_busy}
            onClick={() => void changeAccount('sign_out', signOutOfAccount)}
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
      {pending_conflict && review_open && (
        <Sheet
          open
          title="Choose which model settings to use"
          onClose={() => {
            setReviewing(false);
            setActionError('');
          }}
          dismissible={!busy}
        >
          <ConfigSyncReview
            conflict={pending_conflict}
            providers={providers}
            busy={!!busy}
            disabled={disabled || keys_busy}
            stale={review_stale}
            error={error ?? ''}
            onRefresh={() => void refreshReview()}
            onKeepLocal={() =>
              void runSync({ resolution: 'keep_local', conflict_id: pending_conflict.conflict_id })
            }
            onUseCloud={() =>
              void runSync({ resolution: 'use_cloud', conflict_id: pending_conflict.conflict_id })
            }
          />
        </Sheet>
      )}
    </section>
  );
}
