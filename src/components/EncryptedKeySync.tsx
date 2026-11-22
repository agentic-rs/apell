import { useEffect, useRef, useState } from 'react';
import {
  approveKeySyncPairing,
  cancelKeySyncPairing,
  confirmKeySyncRecovery,
  createKeySync,
  finishKeySyncPairing,
  getKeySyncStatus,
  listKeySyncCopies,
  message,
  recoverKeySync,
  removeKeySyncCopies,
  restoreKeySync,
  startKeySyncPairing,
  uploadKeySync,
} from '../lib/tauri';
import type {
  KeySyncCloudCopies,
  KeySyncCloudCopy,
  KeySyncPairing,
  KeySyncStatus,
  LlmConnection,
  LlmProvider,
} from '../lib/types';
import { OperationScope, type Operation } from './providers/state';
import { Sheet } from './Sheet';

type Flow =
  | { kind: 'setup'; recovery_key?: string }
  | { kind: 'pair'; pairing?: KeySyncPairing }
  | { kind: 'manage'; copies?: KeySyncCloudCopies }
  | { kind: 'recover' | 'approve' | 'upload' | 'restore' };

const titles = {
  setup: 'Set up encrypted API keys',
  pair: 'Pair this device',
  recover: 'Unlock with recovery key',
  approve: 'Approve another device',
  upload: 'Upload encrypted API keys',
  restore: 'Restore encrypted API keys',
  manage: 'Manage cloud copies',
};

const progress = {
  setup: 'Setting up encrypted keys…',
  confirm: 'Confirming recovery key…',
  pair: 'Creating pairing code…',
  recover: 'Unlocking keys…',
  approve: 'Approving device…',
  upload: 'Encrypting and uploading keys…',
  restore: 'Restoring keys…',
  list: 'Loading cloud copies…',
  remove: 'Removing cloud copies…',
};

type Action = keyof typeof progress;

function CopyableCode({ value, label }: { value: string; label: string }) {
  const code = useRef<HTMLElement>(null);
  const [feedback, setFeedback] = useState('');

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setFeedback('Copied.');
    } catch {
      if (code.current) window.getSelection()?.selectAllChildren(code.current);
      setFeedback('Copy the selected text.');
    }
  }

  return (
    <>
      <code className="key-sync-code" ref={code}>
        {value}
      </code>
      <button type="button" onClick={() => void copy()}>
        {label}
      </button>
      {feedback && (
        <p className="status" role="status">
          {feedback}
        </p>
      )}
    </>
  );
}

function PairingApproval({
  pairing,
  onApproved,
}: {
  pairing: KeySyncPairing;
  onApproved: () => void;
}) {
  const approved = useRef(onApproved);
  approved.current = onApproved;
  const [error, setError] = useState('');
  const [expired, setExpired] = useState(false);

  useEffect(() => {
    let active = true;
    let timer: number | undefined;
    const expires_at = Date.parse(pairing.expires_at);

    const poll = async () => {
      if (!Number.isFinite(expires_at) || Date.now() >= expires_at) {
        setExpired(true);
        return;
      }
      try {
        const complete = await finishKeySyncPairing(pairing.pairing_code);
        if (!active) return;
        if (complete) {
          approved.current();
          return;
        }
        setError('');
      } catch (error) {
        if (active) setError(message(error));
      }
      if (active) timer = window.setTimeout(() => void poll(), 3_000);
    };

    timer = window.setTimeout(() => void poll(), 0);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [pairing]);

  if (expired)
    return <p role="alert">This pairing code expired. Close this sheet to start again.</p>;

  return (
    <>
      <p>
        On an unlocked device, open Apell Settings, choose Approve another device, and enter this
        code.
      </p>
      <CopyableCode value={pairing.pairing_code} label="Copy pairing code" />
      <p className="status" role="status">
        Waiting for approval…
      </p>
      {error && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
    </>
  );
}

function CodeEntry({
  label,
  action,
  disabled,
  onSubmit,
}: {
  label: string;
  action: string;
  disabled: boolean;
  onSubmit: (value: string) => void;
}) {
  const [value, setValue] = useState('');
  return (
    <form
      className="transfer-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (value.trim() && !disabled) onSubmit(value.trim());
      }}
    >
      <label>
        {label}
        <input
          type="password"
          value={value}
          required
          disabled={disabled}
          maxLength={320}
          autoCapitalize="none"
          autoCorrect="off"
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => setValue(event.target.value)}
        />
      </label>
      <button type="submit" className="primary" disabled={disabled || !value.trim()}>
        {action}
      </button>
    </form>
  );
}

function UploadSelection({
  connections,
  disabled,
  onUpload,
}: {
  connections: LlmConnection[];
  disabled: boolean;
  onUpload: (connection_ids: string[], replace_existing: boolean) => void;
}) {
  const [selected, setSelected] = useState<string[]>([]);
  const [replace_existing, setReplaceExisting] = useState(false);
  const connection_ids = connections
    .filter((connection) => selected.includes(connection.connection_id))
    .map((connection) => connection.connection_id);

  return (
    <form
      className="transfer-form"
      onSubmit={(event) => {
        event.preventDefault();
        if (connection_ids.length && !disabled) onUpload(connection_ids, replace_existing);
      }}
    >
      <p>Select the connections whose API keys you want to sync.</p>
      <div className="transfer-selection" role="group" aria-label="API keys to upload">
        {connections.map((connection) => (
          <label className="check" key={connection.connection_id}>
            <input
              type="checkbox"
              checked={selected.includes(connection.connection_id)}
              disabled={disabled}
              onChange={(event) => {
                const id = connection.connection_id;
                const checked = event.target.checked;
                setSelected((current) =>
                  checked ? [...current, id] : current.filter((item) => item !== id),
                );
              }}
            />
            <span>{connection.name}</span>
          </label>
        ))}
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={replace_existing}
          disabled={disabled}
          onChange={(event) => setReplaceExisting(event.target.checked)}
        />
        Replace existing cloud copies for selected connections
      </label>
      <button type="submit" className="primary" disabled={disabled || !connection_ids.length}>
        Upload keys
      </button>
    </form>
  );
}

function CloudCopyLabel({ entry }: { entry: KeySyncCloudCopy }) {
  const short_id = entry.connection_id.slice(0, 8);
  return (
    <>
      <strong>{entry.name ?? 'Unmatched connection'}</strong>
      <small>{entry.connection_id.length > short_id.length ? `${short_id}…` : short_id}</small>
    </>
  );
}

function CloudCopySelection({
  copies,
  disabled,
  onRemove,
}: {
  copies: KeySyncCloudCopies;
  disabled: boolean;
  onRemove: (expected_head: string, connection_ids: string[]) => void;
}) {
  const [selected, setSelected] = useState<string[]>([]);
  const [reviewing, setReviewing] = useState(false);
  const remove = copies.entries.filter((entry) => selected.includes(entry.connection_id));
  const revision_id = copies.revision_id;

  if (!revision_id || !copies.entries.length) return <p>There are no saved cloud copies.</p>;

  if (reviewing) {
    return (
      <>
        <p>Remove these encrypted copies from your account?</p>
        <ul className="transfer-preview">
          {remove.map((entry) => (
            <li key={entry.connection_id}>
              <CloudCopyLabel entry={entry} />
            </li>
          ))}
        </ul>
        <div className="button-row">
          <button type="button" disabled={disabled} onClick={() => setReviewing(false)}>
            Back
          </button>
          <button
            type="button"
            className="danger"
            disabled={disabled || !remove.length}
            onClick={() =>
              onRemove(
                revision_id,
                remove.map((entry) => entry.connection_id),
              )
            }
          >
            Remove cloud copies
          </button>
        </div>
      </>
    );
  }

  return (
    <>
      <p>Choose the encrypted cloud copies to remove.</p>
      <div className="transfer-selection" role="group" aria-label="Cloud copies to remove">
        {copies.entries.map((entry) => (
          <label className="check" key={entry.connection_id}>
            <input
              type="checkbox"
              checked={selected.includes(entry.connection_id)}
              disabled={disabled}
              onChange={(event) => {
                const id = entry.connection_id;
                const checked = event.target.checked;
                setSelected((current) =>
                  checked ? [...current, id] : current.filter((item) => item !== id),
                );
              }}
            />
            <span>
              <CloudCopyLabel entry={entry} />
            </span>
          </label>
        ))}
      </div>
      <button
        type="button"
        disabled={disabled || !remove.length}
        onClick={() => setReviewing(true)}
      >
        Review removal
      </button>
    </>
  );
}

function canUpload(connection: LlmConnection, providers: LlmProvider[]) {
  if (
    connection.credential_status !== 'present' ||
    connection.provider_id === 'custom' ||
    connection.provider_id === 'llama-cpp'
  ) {
    return false;
  }
  const provider = providers.find((item) => item.provider_id === connection.provider_id);
  return (
    provider?.base_url.startsWith('https://') &&
    provider.auth_methods.includes('api_key') &&
    !provider.auth_methods.includes('device_code') &&
    provider.base_url.replace(/\/+$/, '') === connection.base_url.replace(/\/+$/, '')
  );
}

export function EncryptedKeySync({
  connections,
  providers,
  disabled,
  onApplied,
  onBusyChange,
}: {
  connections: LlmConnection[];
  providers: LlmProvider[];
  disabled: boolean;
  onApplied: () => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const [status, setStatus] = useState<KeySyncStatus>();
  const [flow, setFlow] = useState<Flow>();
  const [pending, setPending] = useState<Action>();
  const [feedback, setFeedback] = useState('');
  const [read_error, setReadError] = useState('');
  const [action_error, setActionError] = useState('');
  const scope = useRef(new OperationScope());
  const work_pending = useRef(false);
  const active_pairing = useRef<string | undefined>(undefined);
  const read_version = useRef(0);

  function stopPairing() {
    const code = active_pairing.current;
    active_pairing.current = undefined;
    if (code) void cancelKeySyncPairing(code).catch(() => {});
  }

  async function refresh() {
    const version = ++read_version.current;
    setReadError('');
    setStatus(undefined);
    try {
      const result = await getKeySyncStatus();
      if (version === read_version.current) setStatus(result);
    } catch (error) {
      if (version === read_version.current) setReadError(message(error));
    }
  }

  useEffect(() => {
    setFlow(undefined);
    setPending(undefined);
    setActionError('');
    setFeedback('');
    void refresh();
    return () => {
      read_version.current += 1;
      scope.current.cancel();
      stopPairing();
      setFlow(undefined);
      if (work_pending.current) {
        work_pending.current = false;
        onBusyChange(false);
      }
    };
  }, [onBusyChange]);

  async function run(action: Action, work: (operation: Operation) => Promise<void>) {
    if (disabled) return;
    const operation = scope.current.start();
    if (!operation) return;
    setFeedback('');
    setActionError('');
    setPending(action);
    work_pending.current = true;
    onBusyChange(true);
    try {
      await work(operation);
    } catch (error) {
      if (operation.isCurrent()) setActionError(message(error));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setPending(undefined);
        work_pending.current = false;
        onBusyChange(false);
      }
    }
  }

  function open(kind: Flow['kind']) {
    setFeedback('');
    setActionError('');
    setFlow({ kind });
  }

  function loadCloudCopies() {
    void run('list', async (operation) => {
      const copies = await listKeySyncCopies();
      if (operation.isCurrent()) setFlow({ kind: 'manage', copies });
    });
  }

  function finish(text: string) {
    setFlow(undefined);
    setFeedback(text);
  }

  function unlocked() {
    setStatus({ state: 'unlocked' });
    finish('Encrypted API keys unlocked on this device.');
  }

  if (status?.state === 'signed_out') return null;

  const uploadable = connections.filter((connection) => canUpload(connection, providers));
  const busy = disabled || !!pending;

  return (
    <section className="account-key-sync" aria-label="Encrypted API keys">
      <h3>Encrypted API keys</h3>
      {!status &&
        (read_error ? (
          <>
            <p className="status error" role="alert">
              {read_error}
            </p>
            <button type="button" disabled={busy} onClick={() => void refresh()}>
              Check again
            </button>
          </>
        ) : (
          <p role="status">Checking key sync…</p>
        ))}
      {status?.state === 'unavailable' && <p>Encrypted key sync is unavailable in this build.</p>}
      {status?.state === 'setup' && (
        <>
          <p>Encrypt saved API keys before syncing them to your Account.</p>
          <button type="button" disabled={busy} onClick={() => open('setup')}>
            Set up key sync
          </button>
        </>
      )}
      {status?.state === 'locked' && (
        <>
          <p>Unlock key sync on this device.</p>
          <div className="button-row">
            <button type="button" disabled={busy} onClick={() => open('pair')}>
              Pair with existing device
            </button>
            <button type="button" disabled={busy} onClick={() => open('recover')}>
              Use recovery key
            </button>
          </div>
        </>
      )}
      {status?.state === 'unlocked' && (
        <>
          <div className="button-row">
            <button type="button" disabled={busy} onClick={() => open('upload')}>
              Upload keys
            </button>
            <button type="button" disabled={busy} onClick={() => open('restore')}>
              Restore keys
            </button>
          </div>
          <div className="button-row">
            <button type="button" disabled={busy} onClick={() => open('approve')}>
              Approve another device
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                open('manage');
                loadCloudCopies();
              }}
            >
              Manage cloud copies
            </button>
          </div>
        </>
      )}
      {feedback && (
        <p className="status" role="status">
          {feedback}
        </p>
      )}
      {flow && (
        <Sheet
          open
          title={titles[flow.kind]}
          onClose={() => {
            const was_pairing = !!active_pairing.current;
            stopPairing();
            setFlow(undefined);
            if (was_pairing || action_error) void refresh();
          }}
          dismissible={!pending}
        >
          <div className="transfer-form">
            {flow.kind === 'setup' &&
              (flow.recovery_key ? (
                <>
                  <p>
                    Save this recovery key offline. You can use it if you lose access to your
                    approved devices.
                  </p>
                  <CopyableCode value={flow.recovery_key} label="Copy recovery key" />
                  <button
                    type="button"
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void run('confirm', async (operation) => {
                        await confirmKeySyncRecovery();
                        if (operation.isCurrent()) unlocked();
                      })
                    }
                  >
                    I saved the recovery key
                  </button>
                </>
              ) : (
                <>
                  <p>
                    Apell creates a recovery key to unlock encrypted API keys on another device.
                  </p>
                  <button
                    type="button"
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void run('setup', async (operation) => {
                        const { recovery_key } = await createKeySync();
                        if (operation.isCurrent()) setFlow({ kind: 'setup', recovery_key });
                      })
                    }
                  >
                    Show recovery key
                  </button>
                </>
              ))}
            {flow.kind === 'pair' &&
              (flow.pairing ? (
                <PairingApproval
                  pairing={flow.pairing}
                  onApproved={() => {
                    if (flow.pairing?.pairing_code !== active_pairing.current) return;
                    active_pairing.current = undefined;
                    unlocked();
                  }}
                />
              ) : (
                <>
                  <p>
                    Use an unlocked Apell device signed in to this Account to approve this device.
                  </p>
                  <button
                    type="button"
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void run('pair', async (operation) => {
                        const pairing = await startKeySyncPairing();
                        if (!operation.isCurrent()) {
                          void cancelKeySyncPairing(pairing.pairing_code).catch(() => {});
                          return;
                        }
                        active_pairing.current = pairing.pairing_code;
                        setFlow({ kind: 'pair', pairing });
                      })
                    }
                  >
                    Create pairing code
                  </button>
                </>
              ))}
            {flow.kind === 'recover' && (
              <CodeEntry
                label="Recovery key"
                action="Unlock keys"
                disabled={busy}
                onSubmit={(recovery_key) =>
                  void run('recover', async (operation) => {
                    await recoverKeySync(recovery_key);
                    if (operation.isCurrent()) unlocked();
                  })
                }
              />
            )}
            {flow.kind === 'approve' && (
              <>
                <p>Enter the code shown in Apell on the device you want to approve.</p>
                <CodeEntry
                  label="Pairing code"
                  action="Approve device"
                  disabled={busy}
                  onSubmit={(pairing_code) =>
                    void run('approve', async (operation) => {
                      await approveKeySyncPairing(pairing_code);
                      if (operation.isCurrent()) finish('Device approved.');
                    })
                  }
                />
              </>
            )}
            {flow.kind === 'upload' &&
              (uploadable.length ? (
                <UploadSelection
                  connections={uploadable}
                  disabled={busy}
                  onUpload={(connection_ids, replace_existing) =>
                    void run('upload', async (operation) => {
                      const { updated } = await uploadKeySync(connection_ids, replace_existing);
                      if (operation.isCurrent()) {
                        finish(
                          updated
                            ? `Uploaded ${updated} API ${updated === 1 ? 'key' : 'keys'}.`
                            : 'The selected keys are up to date.',
                        );
                      }
                    })
                  }
                />
              ) : (
                <p>Add a saved API key for a built-in provider to upload it.</p>
              ))}
            {flow.kind === 'restore' && (
              <>
                <p>
                  Restore missing API keys for connections saved on this device. You can sync model
                  settings first to bring over connections.
                </p>
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  onClick={() =>
                    void run('restore', async (operation) => {
                      const { updated } = await restoreKeySync();
                      if (updated) onApplied();
                      if (operation.isCurrent()) {
                        finish(
                          updated
                            ? `Restored ${updated} API ${updated === 1 ? 'key' : 'keys'}.`
                            : 'No missing keys could be restored for the saved connections.',
                        );
                      }
                    })
                  }
                >
                  Restore missing keys
                </button>
              </>
            )}
            {flow.kind === 'manage' && (
              <>
                {flow.copies && (
                  <CloudCopySelection
                    key={flow.copies.revision_id ?? 'empty'}
                    copies={flow.copies}
                    disabled={busy}
                    onRemove={(expected_head, connection_ids) =>
                      void run('remove', async (operation) => {
                        const { updated } = await removeKeySyncCopies(
                          expected_head,
                          connection_ids,
                        );
                        if (operation.isCurrent()) {
                          finish(`Removed ${updated} cloud ${updated === 1 ? 'copy' : 'copies'}.`);
                        }
                      })
                    }
                  />
                )}
                {action_error && (
                  <button type="button" disabled={busy} onClick={loadCloudCopies}>
                    Reload cloud copies
                  </button>
                )}
              </>
            )}
            {pending && (
              <p className="status" role="status">
                {progress[pending]}
              </p>
            )}
            {action_error && (
              <p className="status error" role="alert">
                {action_error}
              </p>
            )}
          </div>
        </Sheet>
      )}
    </section>
  );
}
