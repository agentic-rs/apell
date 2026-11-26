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
const expired_request_error = 'This request expired. Start again.';
const expired_approval_error =
  'This pairing code expired. Ask the other device to show a new code.';

type VerificationRequest =
  | { kind: 'join'; pairing: KeySyncPairing }
  | { kind: 'recover' | 'upgrade'; challenge: KeySyncRecovery };
type Flow = VerificationRequest | { kind: 'approve' };
type Progress = 'waiting' | 'expired' | 'rejected' | { error: string };

const titles = {
  join: 'Pair this device',
  approve: 'Approve a device',
  recover: 'Recover access',
  upgrade: 'Verify your Account',
};

function hasExpired(expires_at: number) {
  return expires_at * 1000 <= Date.now();
}

function PairingCode({ code }: { code: string }) {
  return (
    <code className="key-sync-code" aria-label="Pairing code">
      {code}
    </code>
  );
}

function waitUntilComplete(
  expires_at: number,
  finish: () => Promise<boolean | 'rejected'>,
  onComplete: () => void,
  onProgress: (progress: Progress) => void,
): () => void {
  let active = true;
  let timer: number | undefined;
  function schedule() {
    const remaining = expires_at * 1000 - Date.now();
    timer = window.setTimeout(() => void poll(), Math.max(0, Math.min(poll_interval, remaining)));
  }
  async function poll() {
    if (hasExpired(expires_at)) {
      onProgress('expired');
      return;
    }
    try {
      const done = await finish();
      if (!active) return;
      if (done === 'rejected') {
        onProgress('rejected');
        return;
      }
      if (done) {
        onComplete();
        return;
      }
      onProgress('waiting');
    } catch (error) {
      if (!active) return;
      const text = message(error);
      if (text === expired_request_error) {
        onProgress('expired');
        return;
      }
      onProgress({ error: text });
    }
    schedule();
  }
  schedule();
  return () => {
    active = false;
    window.clearTimeout(timer);
  };
}

function Verification({
  request,
  onComplete,
  onOpen,
  onRestart,
  busy,
}: {
  request: VerificationRequest;
  onComplete: () => void;
  onOpen: () => void;
  onRestart: () => void;
  busy: boolean;
}) {
  const [result, setResult] = useState<{ request: VerificationRequest; progress: Progress }>();
  const done = useRef(onComplete);
  done.current = onComplete;
  useEffect(() => {
    const [expires_at, finish] =
      request.kind === 'join'
        ? [request.pairing.expires_at, () => finishKeySyncPairing(request.pairing.pair_id)]
        : [request.challenge.expires_at, finishKeySyncRecovery];
    return waitUntilComplete(
      expires_at,
      finish,
      () => done.current(),
      (progress) => setResult({ request, progress }),
    );
  }, [request]);

  const progress = result?.request === request ? result.progress : 'waiting';
  const pairing = request.kind === 'join' ? request.pairing : undefined;
  if (progress === 'expired' || progress === 'rejected') {
    const feedback =
      progress === 'rejected'
        ? 'Account could not verify this request.'
        : pairing
          ? 'This pairing code expired.'
          : 'Account verification expired.';
    return (
      <>
        <p role="alert" className="status error">
          {feedback}
        </p>
        <button type="button" className="primary" disabled={busy} onClick={onRestart}>
          {busy ? 'Starting…' : pairing ? 'Get a new code' : 'Restart Account verification'}
        </button>
      </>
    );
  }

  return (
    <>
      {pairing ? (
        <>
          <p>
            In <strong>Settings → Account</strong> on a verified device, choose{' '}
            <strong>Approve a device</strong>. Approve only if the six digits match.
          </p>
          <PairingCode code={pairing.code} />
        </>
      ) : (
        <>
          <p>Complete verification in your browser, then return to Apell.</p>
          <button type="button" className="primary" disabled={busy} onClick={onOpen}>
            Open Account verification
          </button>
        </>
      )}
      {typeof progress === 'object' ? (
        <p role="alert" className="status error">
          {progress.error}
        </p>
      ) : (
        <p role="status">{pairing ? 'Waiting for approval…' : 'Waiting for verification…'}</p>
      )}
    </>
  );
}

function Approvals({ onBusyChange }: { onBusyChange: (busy: boolean) => void }) {
  const [pairs, setPairs] = useState<KeySyncPairing[]>();
  const [poll_error, setPollError] = useState('');
  const [approval_error, setApprovalError] = useState('');
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
        setPairs((previous) =>
          previous?.some((pair) => hasExpired(pair.expires_at))
            ? previous.filter((pair) => !hasExpired(pair.expires_at))
            : previous,
        );
        try {
          const result = await listKeySyncPairings();
          if (active && !pending.current) {
            setPairs(
              result.pairs.filter(
                (pair) =>
                  !pair.approved &&
                  !hasExpired(pair.expires_at) &&
                  !dismissed.current.has(pair.pair_id),
              ),
            );
            setPollError('');
          }
        } catch (error) {
          if (active && !pending.current) setPollError(message(error));
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

  function dismissPair(pair_id: string) {
    dismissed.current.add(pair_id);
    setPairs((previous) => previous?.filter((pair) => pair.pair_id !== pair_id));
  }

  async function approve(pair: KeySyncPairing) {
    if (hasExpired(pair.expires_at)) {
      dismissPair(pair.pair_id);
      setApprovalError(expired_approval_error);
      return;
    }
    const operation = scope.current.start();
    if (!operation) return;
    pending.current = true;
    setBusy(pair.pair_id);
    onBusyChange(true);
    setApprovalError('');
    try {
      await approveKeySyncPairing(pair.pair_id, pair.approval_nonce);
      if (!operation.isCurrent()) return;
      dismissPair(pair.pair_id);
      setApproved(true);
    } catch (error) {
      if (operation.isCurrent()) {
        const text = message(error);
        if (text === expired_request_error) dismissPair(pair.pair_id);
        setApprovalError(text === expired_request_error ? expired_approval_error : text);
      }
    } finally {
      pending.current = false;
      onBusyChange(false);
      if (operation.isCurrent()) {
        operation.finish();
        setBusy(undefined);
      }
    }
  }

  const error = approval_error || poll_error;
  return (
    <>
      <p>
        In <strong>Settings → Account</strong> on the new device, choose{' '}
        <strong>Pair this device</strong>. Approve only when the six digits match.
      </p>
      {!pairs && !error && <p role="status">Checking for devices…</p>}
      {pairs?.length === 0 && !error && (
        <p role="status">
          {approved
            ? 'Device approved. It will finish verification.'
            : 'Waiting for a pairing request…'}
        </p>
      )}
      {pairs?.map((pair) => (
        <div className="key-sync-approval" key={pair.pair_id}>
          <PairingCode code={pair.code} />
          <button
            type="button"
            className="primary"
            aria-label={`Approve code ${[...pair.code].join(' ')}`}
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

interface Props {
  disabled: boolean;
  onApplied: () => void;
  onBusyChange: (busy: boolean) => void;
}

async function cancelRequest(flow: Flow | undefined) {
  if (flow?.kind === 'join') {
    await cancelKeySyncPairing(flow.pairing.pair_id).catch(() => {});
  } else if (flow && flow.kind !== 'approve') {
    await cancelKeySyncRecovery().catch(() => {});
  }
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

  function deviceVerified() {
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
    setFlow(undefined);
    setError('');
    void cancelRequest(flow);
  }

  function startRequest(kind: VerificationRequest['kind']) {
    return perform(async (isCurrent) => {
      if (flow) await cancelRequest(flow);
      if (!isCurrent()) return;
      if (kind === 'join') {
        const pairing = await startKeySyncPairing();
        if (isCurrent()) setFlow({ kind, pairing });
      } else {
        const challenge = await startKeySyncRecovery();
        if (!isCurrent()) return;
        setFlow({ kind, challenge });
        await openKeySyncRecovery();
      }
    });
  }

  if (status?.state === 'signed_out') return null;
  const button_disabled = disabled || busy;

  return (
    <section className="account-key-sync" aria-label="Encrypted API keys">
      <h3>Encrypted API keys</h3>
      {!status && !status_error && <p role="status">Checking device…</p>}
      {status?.state === 'unavailable' && (
        <p role="status">Encrypted API-key sync is unavailable in this build.</p>
      )}
      {status?.state === 'verified' && (
        <>
          <p>
            <strong>Verified</strong> · API keys sync automatically.
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
      {status?.state === 'unverified' && (
        <>
          <p>
            <strong>Unverified</strong>
          </p>
          {status.action === 'setup' && (
            <>
              <p>
                Set up encrypted sync for this Account. Eligible API keys sync automatically after
                verification.
              </p>
              <button
                type="button"
                className="primary"
                disabled={button_disabled}
                onClick={() =>
                  void perform(async (isCurrent) => {
                    await createKeySync();
                    if (isCurrent()) deviceVerified();
                  })
                }
              >
                Verify this device
              </button>
            </>
          )}
          {status.action === 'pair' && (
            <>
              <div className="key-sync-option">
                <p>Approve this device from another device that is already verified.</p>
                <button
                  type="button"
                  className="primary"
                  disabled={button_disabled}
                  onClick={() => void startRequest('join')}
                >
                  Pair this device
                </button>
              </div>
              <div className="key-sync-option">
                <p>
                  If you can’t use another verified device, verify your Account again in a browser.
                </p>
                <button
                  type="button"
                  disabled={button_disabled}
                  onClick={() => void startRequest('recover')}
                >
                  Recover access
                </button>
              </div>
            </>
          )}
          {status.action === 'upgrade' && (
            <>
              <p>Verify your Account to keep using your existing encrypted keys.</p>
              <button
                type="button"
                className="primary"
                disabled={button_disabled}
                onClick={() => void startRequest('upgrade')}
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
            {flow.kind === 'approve' && (
              <Approvals
                onBusyChange={(active) => {
                  setApproving(active);
                  onBusyChange(active);
                }}
              />
            )}
            {flow.kind !== 'approve' && (
              <Verification
                request={flow}
                onComplete={deviceVerified}
                busy={busy}
                onOpen={() => void perform(openKeySyncRecovery)}
                onRestart={() => void startRequest(flow.kind)}
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
