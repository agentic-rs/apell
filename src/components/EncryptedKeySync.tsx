import { useEffect, useRef, useState } from 'react';
import {
  approveKeySyncPairing,
  cancelKeySyncPairing,
  cancelKeySyncRecovery,
  createKeySync,
  finishKeySyncPairing,
  finishKeySyncRecovery,
  getKeySyncStatus,
  listKeySyncPairings,
  message,
  onKeySyncChanged,
  openKeySyncRecovery,
  startKeySyncPairing,
  startKeySyncRecovery,
} from '../lib/tauri';
import type { KeySyncPairing, KeySyncRecovery, KeySyncStatus } from '../lib/types';
import { OperationScope } from './providers/state';
import { Sheet } from './Sheet';

const poll_interval = 3_000;

type Flow =
  | { kind: 'join'; pairing: KeySyncPairing }
  | { kind: 'approve' }
  | { kind: 'verify'; challenge: KeySyncRecovery };

const titles = {
  join: 'Pair this device',
  approve: 'Approve a device',
  verify: 'Verify your Account',
};

function PairingCode({ code }: { code: string }) {
  return (
    <code className="key-sync-code" aria-label="Pairing code">
      {code}
    </code>
  );
}

function waitUntilComplete(
  expires_at: number,
  finish: () => Promise<boolean>,
  onComplete: () => void,
  onError: (error: string) => void,
): () => void {
  let active = true;
  let timer: number | undefined;
  async function poll() {
    if (Date.now() >= expires_at * 1000) {
      onError('This request expired. Close this sheet to try again.');
      return;
    }
    try {
      const done = await finish();
      if (!active) return;
      if (done) {
        onComplete();
        return;
      }
      onError('');
    } catch (error) {
      if (active) onError(message(error));
    }
    if (active) timer = window.setTimeout(() => void poll(), poll_interval);
  }
  timer = window.setTimeout(() => void poll(), poll_interval);
  return () => {
    active = false;
    window.clearTimeout(timer);
  };
}

function Joining({ pairing, onPaired }: { pairing: KeySyncPairing; onPaired: () => void }) {
  const [error, setError] = useState('');
  const done = useRef(onPaired);
  done.current = onPaired;
  useEffect(
    () =>
      waitUntilComplete(
        pairing.expires_at,
        () => finishKeySyncPairing(pairing.pair_id),
        () => done.current(),
        setError,
      ),
    [pairing],
  );

  return (
    <>
      <p>
        Open <strong>Approve a device</strong> on a paired device. Approve only if both devices show
        this code.
      </p>
      <PairingCode code={pairing.code} />
      {error ? (
        <p role="alert" className="status error">
          {error}
        </p>
      ) : (
        <p role="status">Waiting for approval…</p>
      )}
    </>
  );
}

function Approvals({ onBusyChange }: { onBusyChange: (busy: boolean) => void }) {
  const [pairs, setPairs] = useState<KeySyncPairing[]>();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState<string>();
  const [approved, setApproved] = useState(false);
  const scope = useRef(new OperationScope());
  const pending = useRef(false);
  const dismissed = useRef(new Set<string>());

  useEffect(() => {
    let active = true;
    let timer: number | undefined;
    async function refresh() {
      if (!pending.current) {
        try {
          const result = await listKeySyncPairings();
          if (active && !pending.current) {
            setPairs(
              result.pairs.filter((pair) => !pair.approved && !dismissed.current.has(pair.pair_id)),
            );
            setError('');
          }
        } catch (error) {
          if (active && !pending.current) setError(message(error));
        }
      }
      if (active) timer = window.setTimeout(() => void refresh(), poll_interval);
    }
    void refresh();
    return () => {
      active = false;
      window.clearTimeout(timer);
      scope.current.cancel();
    };
  }, []);

  async function approve(pair: KeySyncPairing) {
    const operation = scope.current.start();
    if (!operation) return;
    pending.current = true;
    setBusy(pair.pair_id);
    onBusyChange(true);
    setError('');
    try {
      await approveKeySyncPairing(pair.pair_id, pair.approval_nonce);
      if (!operation.isCurrent()) return;
      dismissed.current.add(pair.pair_id);
      setPairs((previous) => previous?.filter((item) => item.pair_id !== pair.pair_id));
      setApproved(true);
    } catch (error) {
      if (operation.isCurrent()) setError(message(error));
    } finally {
      pending.current = false;
      onBusyChange(false);
      if (operation.isCurrent()) {
        operation.finish();
        setBusy(undefined);
      }
    }
  }

  return (
    <>
      <p>
        Choose <strong>Pair this device</strong> on the new device. Approve only when the six digits
        match.
      </p>
      {!pairs && !error && <p role="status">Checking for devices…</p>}
      {pairs?.length === 0 && (
        <p role="status">{approved ? 'Device approved.' : 'Waiting for a pairing request…'}</p>
      )}
      {pairs?.map((pair) => (
        <div className="key-sync-approval" key={pair.pair_id}>
          <PairingCode code={pair.code} />
          <button
            type="button"
            className="primary"
            disabled={!!busy}
            onClick={() => void approve(pair)}
          >
            {busy === pair.pair_id ? 'Approving…' : 'Approve'}
          </button>
        </div>
      ))}
      {error && (
        <p role="alert" className="status error">
          {error}
        </p>
      )}
    </>
  );
}

function Verification({
  challenge,
  onVerified,
  onOpen,
  opening,
}: {
  challenge: KeySyncRecovery;
  onVerified: () => void;
  onOpen: () => void;
  opening: boolean;
}) {
  const [error, setError] = useState('');
  const done = useRef(onVerified);
  done.current = onVerified;
  useEffect(
    () =>
      waitUntilComplete(
        challenge.expires_at,
        finishKeySyncRecovery,
        () => done.current(),
        setError,
      ),
    [challenge],
  );
  return (
    <>
      <p>Complete Account verification in your browser to pair this device.</p>
      <button type="button" className="primary" disabled={opening} onClick={onOpen}>
        Open Account verification
      </button>
      {error ? (
        <p role="alert" className="status error">
          {error}
        </p>
      ) : (
        <p role="status">Waiting for verification…</p>
      )}
    </>
  );
}

interface Props {
  disabled: boolean;
  onApplied: () => void;
  onBusyChange: (busy: boolean) => void;
}

export function EncryptedKeySync({ disabled, onApplied, onBusyChange }: Props) {
  const [status, setStatus] = useState<KeySyncStatus>();
  const [flow, setFlow] = useState<Flow>();
  const [busy, setBusy] = useState(false);
  const [approving, setApproving] = useState(false);
  const [error, setError] = useState('');
  const [status_error, setStatusError] = useState('');
  const scope = useRef(new OperationScope());
  const read_version = useRef(0);

  async function refresh() {
    const version = ++read_version.current;
    try {
      const current = await getKeySyncStatus();
      if (read_version.current !== version) return;
      setStatus(current);
      setStatusError('');
    } catch (error) {
      if (read_version.current === version) setStatusError(message(error));
    }
  }

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    const reload = () => {
      if (active) void refresh();
    };
    reload();
    window.addEventListener('focus', reload);
    void onKeySyncChanged(reload).then(
      (unlisten) => {
        if (active) stop = unlisten;
        else unlisten();
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
  }, []);

  function paired() {
    setFlow(undefined);
    setError('');
    onApplied();
    void refresh();
  }

  async function perform(run: (isCurrent: () => boolean) => Promise<void>) {
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(true);
    onBusyChange(true);
    setError('');
    try {
      await run(operation.isCurrent);
    } catch (error) {
      if (operation.isCurrent()) {
        setError(message(error));
        void refresh();
      }
    } finally {
      onBusyChange(false);
      if (operation.isCurrent()) {
        operation.finish();
        setBusy(false);
      }
    }
  }

  function close() {
    const previous = flow;
    setFlow(undefined);
    setError('');
    if (previous?.kind === 'join')
      void cancelKeySyncPairing(previous.pairing.pair_id).catch(() => {});
    if (previous?.kind === 'verify') void cancelKeySyncRecovery().catch(() => {});
  }

  function startVerification() {
    return perform(async (isCurrent) => {
      const challenge = await startKeySyncRecovery();
      if (!isCurrent()) return;
      setFlow({ kind: 'verify', challenge });
      await openKeySyncRecovery();
    });
  }

  if (status?.state === 'unavailable' || status?.state === 'signed_out') return null;
  const button_disabled = disabled || busy;

  return (
    <section className="account-key-sync" aria-label="Encrypted API keys">
      <h3>Encrypted API keys</h3>
      {!status && !status_error && <p role="status">Checking device…</p>}
      {status?.state === 'paired' && (
        <>
          <p>
            <strong>Paired</strong> · API keys sync automatically.
          </p>
          {status.sync_error && (
            <p className="status error" role="alert">
              {status.sync_error}
            </p>
          )}
          <button
            type="button"
            disabled={button_disabled}
            onClick={() => {
              setError('');
              setFlow({ kind: 'approve' });
            }}
          >
            Approve a device
          </button>
        </>
      )}
      {status?.state === 'unpaired' && (
        <>
          <p>
            <strong>Unpaired</strong>
          </p>
          {(status.action === 'setup' || status.action === 'pair') && (
            <p>Pair this device to sync eligible API keys with this Account.</p>
          )}
          {status.action === 'setup' && (
            <button
              type="button"
              className="primary"
              disabled={button_disabled}
              onClick={() =>
                void perform(async (isCurrent) => {
                  await createKeySync();
                  if (isCurrent()) paired();
                })
              }
            >
              Pair this device
            </button>
          )}
          {status.action === 'pair' && (
            <>
              <button
                type="button"
                className="primary"
                disabled={button_disabled}
                onClick={() =>
                  void perform(async (isCurrent) => {
                    const pairing = await startKeySyncPairing();
                    if (isCurrent()) setFlow({ kind: 'join', pairing });
                  })
                }
              >
                Pair this device
              </button>
              <button
                type="button"
                disabled={button_disabled}
                onClick={() => void startVerification()}
              >
                Recover access
              </button>
            </>
          )}
          {status.action === 'upgrade' && (
            <>
              <p>Verify your Account to keep using your existing encrypted keys.</p>
              <button
                type="button"
                className="primary"
                disabled={button_disabled}
                onClick={() => void startVerification()}
              >
                Verify Account
              </button>
            </>
          )}
          {status.action === 'legacy' && (
            <p>
              Open Apell on a device that already has your keys to finish setting up Account
              recovery.
            </p>
          )}
        </>
      )}
      {status_error && (
        <>
          <p className="status error" role="alert">
            {status_error}
          </p>
          <button type="button" disabled={busy} onClick={() => void refresh()}>
            Check again
          </button>
        </>
      )}
      {!flow && error && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
      {flow && (
        <Sheet open title={titles[flow.kind]} onClose={close} dismissible={!busy && !approving}>
          <div className="transfer-form">
            {flow.kind === 'join' && <Joining pairing={flow.pairing} onPaired={paired} />}
            {flow.kind === 'approve' && (
              <Approvals
                onBusyChange={(active) => {
                  setApproving(active);
                  onBusyChange(active);
                }}
              />
            )}
            {flow.kind === 'verify' && (
              <Verification
                challenge={flow.challenge}
                onVerified={paired}
                opening={busy}
                onOpen={() => void perform(openKeySyncRecovery)}
              />
            )}
            {error && (
              <p className="status error" role="alert">
                {error}
              </p>
            )}
          </div>
        </Sheet>
      )}
    </section>
  );
}
