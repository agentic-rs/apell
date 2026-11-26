import { useEffect, useRef, useState } from 'react';
import {
  applySettingsTransfer,
  exportSettingsTransfer,
  getSettingsTransferCapabilities,
  message,
  previewSettingsTransfer,
  saveSettingsTransferFile,
  scanSettingsTransferQr,
  showSettingsTransferQr,
  usesMockCredentials,
} from '../lib/tauri';
import type {
  LlmConnection,
  LlmProvider,
  SettingsTransferCapabilities,
  SettingsTransferExport,
  SettingsTransferPreview,
  SettingsTransferResult,
} from '../lib/types';
import { Sheet } from './Sheet';
import { OperationScope, type Operation } from './providers/state';

const MAX_BACKUP_BYTES = 256 * 1024;
const MAX_PASSPHRASE_BYTES = 256;

const progress_text = {
  encrypt: 'Encrypting backup…',
  save: 'Saving backup…',
  show: 'Showing QR code…',
  file: 'Reading backup…',
  scan: 'Scanning QR code…',
  preview: 'Unlocking backup…',
  apply: 'Importing connections…',
};

type TransferAction = keyof typeof progress_text;
type TransferMode = 'export' | 'import';
type ConnectionsStatus = 'loading' | 'failed' | 'ready';

interface TransferProps {
  connections: LlmConnection[];
  connections_status: ConnectionsStatus;
  providers: LlmProvider[];
  disabled: boolean;
  onRetry: () => void;
  onImported: () => void;
}

interface TransferFormProps {
  capabilities?: SettingsTransferCapabilities;
  busy: boolean;
  run: (kind: TransferAction, work: (operation: Operation) => Promise<void>) => Promise<void>;
  announce: (text: string) => void;
}

function validPassphrase(value: string): boolean {
  return value.length > 0 && new TextEncoder().encode(value).length <= MAX_PASSPHRASE_BYTES;
}

function PassphraseField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label>
      {label}
      <input
        type="password"
        required
        maxLength={MAX_PASSPHRASE_BYTES}
        value={value}
        autoCapitalize="none"
        autoCorrect="off"
        autoComplete="off"
        spellCheck={false}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

function ExportStatus({
  connections_status,
  disabled,
  onRetry,
}: Pick<TransferProps, 'connections_status' | 'disabled' | 'onRetry'>) {
  if (connections_status === 'ready') return null;
  if (connections_status === 'loading') {
    return (
      <p className="status" role="status">
        Loading connections for export…
      </p>
    );
  }
  return (
    <div>
      <p className="status error" role="alert">
        Couldn’t load connections for export.
      </p>
      <button type="button" disabled={disabled} onClick={onRetry}>
        Try again
      </button>
    </div>
  );
}

function ExportForm({
  connections,
  connections_status,
  capabilities,
  busy,
  run,
  announce,
  onRetry,
}: TransferFormProps & Pick<TransferProps, 'connections' | 'connections_status' | 'onRetry'>) {
  const [selected, setSelected] = useState(() =>
    connections.map((connection) => connection.connection_id),
  );
  const [passphrase, setPassphrase] = useState('');
  const [backup, setBackup] = useState<SettingsTransferExport>();
  const heading = useRef<HTMLHeadingElement>(null);
  const selected_ids = connections
    .map((connection) => connection.connection_id)
    .filter((id) => selected.includes(id));
  const can_export =
    connections_status === 'ready' && selected_ids.length > 0 && validPassphrase(passphrase);
  useEffect(() => {
    if (backup) heading.current?.focus();
  }, [backup]);

  const encrypt = () => {
    if (!can_export) return;
    void run('encrypt', async (operation) => {
      const encrypted = await exportSettingsTransfer(selected_ids, passphrase);
      if (!operation.isCurrent()) return;
      setPassphrase('');
      setBackup(encrypted);
    });
  };

  if (backup) {
    const qr_available = capabilities?.qr && backup.qr_available;
    return (
      <div className="transfer-form">
        <h3 ref={heading} tabIndex={-1}>
          Backup ready
        </h3>
        <p>Use the same passphrase to unlock this backup on the receiving device.</p>
        {qr_available && capabilities.capture_protection === 'hide' && (
          <p className="notice">
            iOS can take a screenshot of the encrypted QR code. Apell hides the code during
            recording and mirroring.
          </p>
        )}
        {qr_available && capabilities.capture_protection === 'block' && (
          <p className="notice">Android blocks screenshots while the QR code is visible.</p>
        )}
        {capabilities?.qr && !backup.qr_available && (
          <p className="notice">
            This backup is too large for one QR code. Save a file or select fewer connections.
          </p>
        )}
        <div className="button-row">
          <button
            type="button"
            className="primary"
            disabled={busy}
            onClick={() =>
              void run('save', async (operation) => {
                const saved = await saveSettingsTransferFile(backup.payload);
                if (operation.isCurrent() && saved) announce('Encrypted backup saved.');
              })
            }
          >
            Save encrypted file
          </button>
          {qr_available && (
            <button
              type="button"
              disabled={busy}
              onClick={() => void run('show', () => showSettingsTransferQr(backup.payload))}
            >
              Show QR code
            </button>
          )}
        </div>
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setBackup(undefined);
            announce('');
          }}
        >
          Change backup options
        </button>
      </div>
    );
  }

  return (
    <form
      className="transfer-form"
      onSubmit={(event) => {
        event.preventDefault();
        encrypt();
      }}
    >
      <fieldset disabled={busy}>
        <div className="transfer-selection" role="group" aria-label="Connections to export">
          {connections.map((connection) => (
            <label key={connection.connection_id} className="check">
              <input
                type="checkbox"
                checked={selected.includes(connection.connection_id)}
                onChange={(event) => {
                  const checked = event.target.checked;
                  setSelected((current) =>
                    checked
                      ? [...current, connection.connection_id]
                      : current.filter((id) => id !== connection.connection_id),
                  );
                }}
              />
              <span>
                <strong>{connection.name}</strong>
                <small>{connection.model_id || 'No model selected'}</small>
              </span>
            </label>
          ))}
        </div>
        <p>
          Enter a passphrase and use it again on the receiving device. The backup includes saved API
          keys. Connections that use account sign-in require a new sign-in after import.
        </p>
        <PassphraseField label="Backup passphrase" value={passphrase} onChange={setPassphrase} />
        {passphrase && new TextEncoder().encode(passphrase).length > MAX_PASSPHRASE_BYTES && (
          <p className="status error" role="status">
            Use a shorter passphrase.
          </p>
        )}
        <ExportStatus connections_status={connections_status} disabled={busy} onRetry={onRetry} />
        <button type="submit" className="primary" disabled={!can_export}>
          Encrypt backup
        </button>
      </fieldset>
    </form>
  );
}

function ImportForm({
  capabilities,
  providers,
  busy,
  run,
  announce,
  onImported,
  onClose,
}: TransferFormProps & Pick<TransferProps, 'providers' | 'onImported'> & { onClose: () => void }) {
  const [source, setSource] = useState<{ payload: string; name: string }>();
  const [passphrase, setPassphrase] = useState('');
  const [preview, setPreview] = useState<SettingsTransferPreview>();
  const [result, setResult] = useState<SettingsTransferResult>();
  const heading = useRef<HTMLHeadingElement>(null);
  const request = source && { payload: source.payload, passphrase };

  useEffect(() => {
    if (preview || result) heading.current?.focus();
  }, [preview, result]);

  const selectSource = (payload: string, name: string) => {
    if (!payload) throw new Error('This backup is empty.');
    setSource({ payload, name });
    setPassphrase('');
  };
  const readFile = (file: File) =>
    void run('file', async (operation) => {
      if (file.size > MAX_BACKUP_BYTES) throw new Error('This backup exceeds 256 KiB.');
      const payload = await file.text();
      if (operation.isCurrent()) selectSource(payload, file.name);
    });
  const scan = () =>
    void run('scan', async (operation) => {
      const payload = await scanSettingsTransferQr();
      if (operation.isCurrent() && payload !== null) selectSource(payload, 'Scanned QR code');
    });
  const unlock = () => {
    if (!request || !validPassphrase(passphrase)) return;
    void run('preview', async (operation) => {
      const unlocked = await previewSettingsTransfer(request);
      if (operation.isCurrent()) setPreview(unlocked);
    });
  };
  const apply = () => {
    if (!request || !preview?.connections.some((connection) => !connection.duplicate)) return;
    void run('apply', async (operation) => {
      const imported = await applySettingsTransfer(request);
      if (!operation.isCurrent()) return;
      setSource(undefined);
      setPassphrase('');
      setPreview(undefined);
      setResult(imported);
      onImported();
    });
  };

  if (result) {
    return (
      <div className="transfer-form">
        <h3 ref={heading} tabIndex={-1}>
          Import complete
        </h3>
        <p role="status">
          {result.added} {result.added === 1 ? 'connection' : 'connections'} imported.
          {result.skipped > 0 && ` ${result.skipped} already saved.`}
        </p>
        <button type="button" className="primary" onClick={onClose}>
          Done
        </button>
      </div>
    );
  }

  if (preview) {
    const additions = preview.connections.filter((connection) => !connection.duplicate).length;
    return (
      <div className="transfer-form">
        <h3 ref={heading} tabIndex={-1}>
          Review connections
        </h3>
        <ul className="transfer-preview">
          {preview.connections.map((connection, index) => (
            <li key={index}>
              <strong>{connection.name}</strong>
              <span>
                {providers.find((provider) => provider.provider_id === connection.provider_id)
                  ?.name ?? connection.provider_id}{' '}
                · {connection.model_id || 'No model selected'}
              </span>
              <span>{connection.base_url}</span>
              <small>
                {connection.duplicate
                  ? 'Already saved · Skip'
                  : connection.credential === 'api_key'
                    ? 'API key included'
                    : connection.credential === 'sign_in'
                      ? 'Sign in after import'
                      : 'No credential included'}
              </small>
            </li>
          ))}
        </ul>
        <div className="button-row">
          {additions > 0 ? (
            <button type="button" className="primary" disabled={busy} onClick={apply}>
              Import {additions} {additions === 1 ? 'connection' : 'connections'}
            </button>
          ) : (
            <button type="button" className="primary" onClick={onClose}>
              Done
            </button>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setPreview(undefined);
              setPassphrase('');
              setSource(undefined);
              announce('');
            }}
          >
            Choose another backup
          </button>
        </div>
      </div>
    );
  }

  return (
    <form
      className="transfer-form"
      onSubmit={(event) => {
        event.preventDefault();
        unlock();
      }}
    >
      <fieldset disabled={busy}>
        <p>Choose an .apell-settings file or scan its QR code, then enter the backup passphrase.</p>
        <div className="button-row">
          <label className={`transfer-file file-option${busy ? ' disabled' : ''}`}>
            Choose backup file
            <input
              type="file"
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = '';
                if (file) readFile(file);
              }}
            />
          </label>
          {capabilities?.qr && (
            <button type="button" onClick={scan}>
              Scan QR code
            </button>
          )}
        </div>
        {source && (
          <>
            <p className="status">Selected: {source.name}</p>
            <PassphraseField
              label="Backup passphrase"
              value={passphrase}
              onChange={setPassphrase}
            />
            {passphrase && new TextEncoder().encode(passphrase).length > MAX_PASSPHRASE_BYTES && (
              <p className="status error" role="status">
                Use a shorter passphrase.
              </p>
            )}
            <button type="submit" className="primary" disabled={!validPassphrase(passphrase)}>
              Preview import
            </button>
          </>
        )}
      </fieldset>
    </form>
  );
}

function TransferSheet({
  mode,
  connections,
  connections_status,
  providers,
  onRetry,
  onClose,
  onImported,
}: Omit<TransferProps, 'disabled'> & {
  mode: TransferMode;
  onClose: () => void;
}) {
  const [capabilities, setCapabilities] = useState<SettingsTransferCapabilities>();
  const [pending, setPending] = useState<TransferAction>();
  const [feedback, setFeedback] = useState<{ text: string; failed: boolean }>();
  const scope = useRef(new OperationScope());
  useEffect(() => {
    let closed = false;
    void getSettingsTransferCapabilities()
      .then((value) => {
        if (!closed) setCapabilities(value);
      })
      .catch(() => {});
    return () => {
      closed = true;
      scope.current.cancel();
    };
  }, []);

  const run: TransferFormProps['run'] = async (kind, work) => {
    const operation = scope.current.start();
    if (!operation) return;
    setFeedback(undefined);
    setPending(kind);
    try {
      await work(operation);
    } catch (error) {
      if (operation.isCurrent()) setFeedback({ text: message(error), failed: true });
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setPending(undefined);
      }
    }
  };
  const form_props: TransferFormProps = {
    capabilities,
    busy: !!pending,
    run,
    announce: (text) => setFeedback(text ? { text, failed: false } : undefined),
  };

  return (
    <Sheet
      open
      title={mode === 'export' ? 'Export connections' : 'Import connections'}
      onClose={() => {
        scope.current.cancel();
        onClose();
      }}
      dismissible={pending !== 'apply'}
    >
      {mode === 'export' ? (
        <ExportForm
          {...form_props}
          connections={connections}
          connections_status={connections_status}
          onRetry={onRetry}
        />
      ) : (
        <>
          {usesMockCredentials() && (
            <p className="notice">
              Import test backups only. Imported API keys are saved in unencrypted files.
            </p>
          )}
          <ImportForm
            {...form_props}
            providers={providers}
            onImported={onImported}
            onClose={onClose}
          />
        </>
      )}
      {pending && (
        <p className="status" role="status">
          {progress_text[pending]}
        </p>
      )}
      {feedback && (
        <p
          className={`status${feedback.failed ? ' error' : ''}`}
          role={feedback.failed ? 'alert' : 'status'}
        >
          {feedback.text}
        </p>
      )}
    </Sheet>
  );
}

export function SettingsTransfer({
  connections,
  connections_status,
  providers,
  disabled,
  onRetry,
  onImported,
}: TransferProps) {
  const [mode, setMode] = useState<TransferMode>();
  return (
    <section className="panel settings-transfer" aria-labelledby="settings-backup">
      <h2 id="settings-backup" className="settings-section-title" tabIndex={-1}>
        Transfer connections
      </h2>
      <p>Move your provider settings and saved API keys with an encrypted backup.</p>
      <div className="button-row">
        <button type="button" disabled={disabled} onClick={() => setMode('import')}>
          Import
        </button>
        <button
          type="button"
          disabled={disabled || connections_status !== 'ready' || !connections.length}
          onClick={() => setMode('export')}
        >
          Export
        </button>
      </div>
      <ExportStatus connections_status={connections_status} disabled={disabled} onRetry={onRetry} />
      {mode && (
        <TransferSheet
          key={mode}
          mode={mode}
          connections={connections}
          connections_status={connections_status}
          providers={providers}
          onRetry={onRetry}
          onClose={() => setMode(undefined)}
          onImported={onImported}
        />
      )}
    </section>
  );
}
