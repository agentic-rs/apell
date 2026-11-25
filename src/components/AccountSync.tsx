import { useCallback, useEffect, useRef, useState } from 'react';
import {
  cancelAccountSignIn,
  getAccountStatus,
  getConfigSyncStatus,
  message,
  onAccountChanged,
  openAccountSignIn,
  setConfigSyncEnabled,
  signInToAccount,
  signOutOfAccount,
  syncConfig,
} from '../lib/tauri';
import type {
  AccountStatus,
  ConfigSyncResolution,
  ConfigSyncResult,
  ConfigSyncStatus,
  ConfigSyncSummary,
  LlmProvider,
} from '../lib/types';
import { AccountSignIn } from './AccountSignIn';
import { EncryptedKeySync } from './EncryptedKeySync';
import { OperationScope } from './providers/state';
import { Sheet } from './Sheet';

type SyncConflict = Extract<ConfigSyncResult, { state: 'conflict' }>;
type Action = 'sign_in' | 'open_sign_in' | 'cancel' | 'sign_out' | 'toggle' | 'sync';

const sync_messages = {
  current: 'Settings are up to date.',
  uploaded: 'Settings saved to your account.',
  downloaded: 'Synced settings applied to this device.',
};

function ConfigVersion({
  title,
  summary,
  providers,
}: {
  title: string;
  summary: ConfigSyncSummary;
  providers: LlmProvider[];
}) {
  return (
    <section className="sync-version">
      <h3>{title}</h3>
      <p>
        {summary.active_model ? `Preferred model · ${summary.active_model}` : 'No preferred model'}
      </p>
      {summary.connections.length ? (
        <ul>
          {summary.connections.map((connection, index) => {
            const provider_name = providers.find(
              (provider) => provider.provider_id === connection.provider_id,
            )?.name;
            return (
              <li key={index}>
                <strong>{connection.name}</strong>
                <span>
                  {provider_name ?? connection.provider_id} · {connection.model_id}
                </span>
              </li>
            );
          })}
        </ul>
      ) : (
        <p>No connections to sync.</p>
      )}
    </section>
  );
}

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
  const [conflict, setConflict] = useState<SyncConflict>();
  const [outcome, setOutcome] = useState('');
  const [read_error, setReadError] = useState('');
  const [action_error, setActionError] = useState('');
  const scope = useRef(new OperationScope());
  const read_version = useRef(0);
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
      setAccount(current);
      if (current.state === 'signed_in') {
        const synced = await getConfigSyncStatus();
        if (read_version.current !== version) return;
        setSync(synced);
        if (synced.state !== 'ready') setConflict(undefined);
      } else {
        setSync(undefined);
        setConflict(undefined);
        setOutcome('');
      }
      setReadError('');
    } catch (error) {
      if (read_version.current === version) setReadError(message(error));
    }
  }

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    setBusy(undefined);
    setConflict(undefined);
    const reload = () => {
      if (active) void refresh();
    };
    reload();
    window.addEventListener('focus', reload);
    void onAccountChanged(reload).then(
      (unlisten) => {
        if (!active) {
          unlisten();
          return;
        }
        stop = unlisten;
        reload();
      },
      () => {},
    );
    return () => {
      active = false;
      window.removeEventListener('focus', reload);
      stop?.();
      read_version.current += 1;
      scope.current.cancel();
    };
  }, [onSyncBusyChange]);

  async function perform(action: Action, run: (isCurrent: () => boolean) => Promise<void>) {
    if (keys_busy) return;
    const operation = scope.current.start();
    if (!operation) return;
    read_version.current += 1;
    setBusy(action);
    setActionError('');
    setOutcome('');
    const changes_settings = action === 'sync' || action === 'toggle';
    if (changes_settings) onSyncBusyChange(true);
    try {
      await run(operation.isCurrent);
    } catch (error) {
      if (operation.isCurrent()) setActionError(message(error));
    } finally {
      if (changes_settings) onSyncBusyChange(false);
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
      if (!enabled) setConflict(undefined);
      if (enabled && next.state === 'ready' && !next.last_synced_at) {
        setBusy('sync');
        await syncSettings(isCurrent);
      }
    });
  }

  function runSync(resolution?: ConfigSyncResolution) {
    return perform('sync', (isCurrent) => syncSettings(isCurrent, resolution));
  }

  async function syncSettings(isCurrent: () => boolean, resolution?: ConfigSyncResolution) {
    const result = await syncConfig(resolution);
    // The native save can finish while Settings is hidden. Refresh the
    // connection list independently of the panel's transient feedback.
    if (result.state === 'downloaded') onApplied();
    if (!isCurrent()) return;
    setSync(result.status);
    if (result.state === 'conflict') {
      setConflict(result);
      return;
    }
    setConflict(undefined);
    setOutcome(sync_messages[result.state]);
  }

  const last_sync = sync?.last_synced_at ? new Date(sync.last_synced_at) : null;
  const last_sync_label =
    last_sync && Number.isFinite(last_sync.getTime()) ? last_sync.toLocaleString() : undefined;
  const error = action_error || read_error || account?.error;

  return (
    <section
      className="panel account-sync"
      aria-busy={!!busy || keys_busy || (!account && !read_error)}
    >
      <h2>Apell Account</h2>
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
          <div className="account-sync-identity">
            <span>{account.email ?? 'Signed in'}</span>
            <button
              type="button"
              disabled={!!busy || keys_busy}
              onClick={() => void changeAccount('sign_out', signOutOfAccount)}
            >
              {busy === 'sign_out' ? 'Signing out…' : 'Sign out'}
            </button>
          </div>
          {sync && sync.state !== 'disabled' && (
            <section className="account-model-sync" aria-label="Model settings sync">
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
              {sync.state === 'ready' && (
                <div className="account-model-sync-row">
                  <button
                    type="button"
                    disabled={disabled || !!busy || keys_busy}
                    onClick={() => void runSync()}
                  >
                    Sync now
                  </button>
                  {last_sync_label && (
                    <p>
                      Cloud updated{' '}
                      <time dateTime={sync.last_synced_at ?? undefined}>{last_sync_label}</time>
                    </p>
                  )}
                </div>
              )}
              {outcome && (
                <p className="status" role="status">
                  {outcome}
                </p>
              )}
            </section>
          )}
          {!sync && !read_error && <p role="status">Checking sync…</p>}
          {sync?.state === 'disabled' && <p>Settings sync is unavailable in this build.</p>}
          <EncryptedKeySync
            disabled={disabled || !!busy || !!conflict}
            onApplied={onApplied}
            onBusyChange={onKeySyncBusyChange}
          />
        </>
      )}
      {error && !conflict && account?.state !== 'signing_in' && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
      {read_error && !busy && (
        <button type="button" onClick={() => void refresh()}>
          Check again
        </button>
      )}
      {conflict && (
        <Sheet
          open
          title="Choose settings to keep"
          onClose={() => {
            setConflict(undefined);
            setActionError('');
          }}
          dismissible={!busy}
        >
          <div className="transfer-form">
            <p>This device and your account have different saved model settings.</p>
            <div className="sync-versions">
              <ConfigVersion title="This device" summary={conflict.local} providers={providers} />
              <ConfigVersion
                title="Synced settings"
                summary={conflict.cloud}
                providers={providers}
              />
            </div>
            <p>
              Using synced settings replaces the listed connections on this device. Removed
              connections lose their saved credentials here.
            </p>
            {error && (
              <p className="status error" role="alert">
                {error}
              </p>
            )}
            <div className="button-row">
              <button
                type="button"
                disabled={disabled || !!busy}
                onClick={() =>
                  void runSync({ resolution: 'keep_local', conflict_id: conflict.conflict_id })
                }
              >
                Keep this device
              </button>
              <button
                type="button"
                disabled={disabled || !!busy}
                onClick={() =>
                  void runSync({ resolution: 'use_cloud', conflict_id: conflict.conflict_id })
                }
              >
                Use synced settings
              </button>
            </div>
            {busy === 'sync' && <p role="status">Syncing…</p>}
          </div>
        </Sheet>
      )}
    </section>
  );
}
