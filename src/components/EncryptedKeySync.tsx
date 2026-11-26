import { useCallback, useEffect, useRef, useState } from 'react';
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
  retryKeySync,
  startKeySyncPairing,
  startKeySyncRecovery,
} from '../lib/tauri';
import type { KeySyncPairing, KeySyncRecovery, KeySyncStatus, LlmProvider } from '../lib/types';
import { KeySyncConflictReview } from './KeySyncConflictReview';
import { OperationScope } from './providers/state';
import { Sheet } from './Sheet';

const poll_interval = 3_000;
const expired_request_error = 'This request expired. Start again.';
const expired_approval_error =
  'This pairing code expired. Ask the other device to show a new code.';

type VerificationRequest =
  | { kind: 'join'; pairing: KeySyncPairing }
  | { kind: 'recover' | 'upgrade'; challenge: KeySyncRecovery };
type Flow = VerificationRequest | { kind: 'approve' } | { kind: 'review' };
type Progress = 'waiting' | 'expired' | 'rejected' | { error: string };

const titles = {
  join: 'Pair this device',
  approve: 'Approve a device',
  recover: 'Recover access',
  upgrade: 'Verify your Account',
  review: 'Review key conflicts',
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
  isCurrent: () => boolean,
): () => void {
  let active = true;
  let timer: number | undefined;
  function schedule() {
    const remaining = expires_at * 1000 - Date.now();
    timer = window.setTimeout(() => void poll(), Math.max(0, Math.min(poll_interval, remaining)));
  }
  async function poll() {
    if (!active || !isCurrent()) return;
    if (hasExpired(expires_at)) {
      onProgress('expired');
      return;
    }
    try {
      const done = await finish();
      if (!active || !isCurrent()) return;
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
      if (!active || !isCurrent()) return;
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
  account_session_id,
  request,
  isCurrent,
  onComplete,
  onOpen,
  onRestart,
  busy,
}: {
  account_session_id: string;
  request: VerificationRequest;
  isCurrent: (request: Flow) => boolean;
  onComplete: (request: VerificationRequest) => void;
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
        ? [
            request.pairing.expires_at,
            () => finishKeySyncPairing(account_session_id, request.pairing.pair_id),
          ]
        : [
            request.challenge.expires_at,
            () => finishKeySyncRecovery(account_session_id, request.challenge.request_id),
          ];
    return waitUntilComplete(
      expires_at,
      finish,
      () => done.current(request),
      (progress) => setResult({ request, progress }),
      () => isCurrent(request),
    );
  }, [account_session_id, request, isCurrent]);

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

function Approvals({
  account_session_id,
  request,
  isCurrent,
  onBusyChange,
}: {
  account_session_id: string;
  request: Extract<Flow, { kind: 'approve' }>;
  isCurrent: (request: Flow) => boolean;
  onBusyChange: (busy: boolean) => void;
}) {
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
      if (!active || !isCurrent(request)) return;
      if (!pending.current) {
        setPairs((previous) =>
          previous?.some((pair) => hasExpired(pair.expires_at))
            ? previous.filter((pair) => !hasExpired(pair.expires_at))
            : previous,
        );
        try {
          const result = await listKeySyncPairings(account_session_id);
          if (active && isCurrent(request) && !pending.current) {
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
          if (active && isCurrent(request) && !pending.current) setPollError(message(error));
        }
      }
      if (active && isCurrent(request)) {
        timer = window.setTimeout(() => void refresh(), poll_interval);
      }
    }
    void refresh();
    return () => {
      active = false;
      window.clearTimeout(timer);
      scope.current.cancel();
    };
  }, [account_session_id, request, isCurrent]);

  function dismissPair(pair_id: string) {
    dismissed.current.add(pair_id);
    setPairs((previous) => previous?.filter((pair) => pair.pair_id !== pair_id));
  }

  async function approve(pair: KeySyncPairing) {
    if (!isCurrent(request)) return;
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
      await approveKeySyncPairing(account_session_id, pair.pair_id, pair.approval_nonce);
      if (!operation.isCurrent() || !isCurrent(request)) return;
      dismissPair(pair.pair_id);
      setApproved(true);
    } catch (error) {
      if (operation.isCurrent() && isCurrent(request)) {
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
  account_session_id: string;
  providers: LlmProvider[];
  disabled: boolean;
  model_sync_enabled?: boolean;
  model_review_needed: boolean;
  onApplied: () => void;
  onBusyChange: (busy: boolean) => void;
}

const sync_labels = {
  pending: 'Waiting to sync',
  syncing: 'Syncing keys…',
  current: 'Keys are up to date',
  waiting_for_model_settings: 'Waiting for model settings',
  conflict: 'Local and cloud keys differ',
  failed: 'Couldn’t sync keys',
  paused: 'Sync paused on this device',
} satisfies Record<Extract<KeySyncStatus, { state: 'verified' }>['sync_state'], string>;

function ModelSettingsRecovery({
  enabled,
  review_needed,
}: {
  enabled?: boolean;
  review_needed: boolean;
}) {
  if (review_needed) {
    return (
      <p>
        Choose <strong>Review changes</strong> above to check the provider connections for these
        keys.
      </p>
    );
  }
  if (enabled === false) {
    return (
      <p>
        Turn on <strong>Sync model settings</strong> above to restore the provider connections for
        your backed-up keys.
      </p>
    );
  }
  if (enabled === true) {
    return (
      <p>
        Check <strong>Model settings</strong> on the device with these connections. Turn on sync
        there and review any pending changes. Then retry key sync here.
      </p>
    );
  }
  return <p>Check Model settings above to restore the connections for your backed-up keys.</p>;
}

async function cancelRequest(account_session_id: string, flow: Flow | undefined) {
  if (flow?.kind === 'join') {
    await cancelKeySyncPairing(account_session_id, flow.pairing.pair_id).catch(() => {});
  } else if (flow?.kind === 'recover' || flow?.kind === 'upgrade') {
    await cancelKeySyncRecovery(account_session_id, flow.challenge.request_id).catch(() => {});
  }
}

export function EncryptedKeySync({
  account_session_id,
  providers,
  disabled,
  model_sync_enabled,
  model_review_needed,
  onApplied,
  onBusyChange,
}: Props) {
  const [status, setStatus] = useState<KeySyncStatus>();
  const [flow, setFlow] = useState<Flow>();
  const active_flow = useRef<Flow | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [approving, setApproving] = useState(false);
  const [resolving, setResolving] = useState(false);
  const resolution_pending = useRef(false);
  const [error, setError] = useState('');
  const [status_error, setStatusError] = useState('');
  const [checking, setChecking] = useState(true);
  const [retrying, setRetrying] = useState(false);
  const [retry_error, setRetryError] = useState('');
  const scope = useRef(new OperationScope());
  const retry_scope = useRef(new OperationScope());
  const read_version = useRef(0);
  const visible = useRef(false);
  const isCurrentFlow = useCallback((request: Flow) => active_flow.current === request, []);

  function showFlow(next?: Flow) {
    active_flow.current = next;
    setFlow(next);
  }

  async function refresh() {
    if (!visible.current) return;
    const version = ++read_version.current;
    setChecking(true);
    try {
      const current = await getKeySyncStatus(account_session_id);
      if (read_version.current !== version) return;
      if (
        current.state === 'signed_out' ||
        (active_flow.current?.kind === 'review' && current.state !== 'verified')
      ) {
        showFlow();
      }
      setStatus(current);
      setStatusError('');
    } catch (error) {
      if (read_version.current === version) setStatusError(message(error));
    } finally {
      if (read_version.current === version) setChecking(false);
    }
  }

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    visible.current = true;
    setRetrying(false);
    const reload = () => {
      if (active) void refresh();
    };
    window.addEventListener('focus', reload);
    void onKeySyncChanged(reload).then((unlisten) => {
      if (active) {
        stop = unlisten;
        reload();
      } else unlisten();
    }, reload);
    return () => {
      active = false;
      visible.current = false;
      window.removeEventListener('focus', reload);
      stop?.();
      read_version.current += 1;
      scope.current.cancel();
      retry_scope.current.cancel();
    };
  }, [account_session_id]);

  async function retry() {
    const operation = retry_scope.current.start();
    if (!operation) return;
    setRetrying(true);
    setRetryError('');
    try {
      await retryKeySync(account_session_id);
      if (operation.isCurrent()) void refresh();
    } catch (cause) {
      if (operation.isCurrent()) setRetryError(message(cause));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setRetrying(false);
      }
    }
  }

  function deviceVerified(request?: VerificationRequest) {
    if (request && !isCurrentFlow(request)) return;
    showFlow();
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
    if (resolution_pending.current) return;
    const current = active_flow.current;
    showFlow();
    setError('');
    void cancelRequest(account_session_id, current);
  }

  function startRequest(kind: VerificationRequest['kind'], replacing?: VerificationRequest) {
    if (active_flow.current !== replacing) return;
    return perform(async (isCurrent) => {
      if (replacing) await cancelRequest(account_session_id, replacing);
      if (!isCurrent() || active_flow.current !== replacing) return;
      const next: VerificationRequest =
        kind === 'join'
          ? { kind, pairing: await startKeySyncPairing(account_session_id) }
          : { kind, challenge: await startKeySyncRecovery(account_session_id) };
      if (!isCurrent() || active_flow.current !== replacing) {
        void cancelRequest(account_session_id, next);
        return;
      }
      showFlow(next);
      if (next.kind !== 'join')
        await openKeySyncRecovery(account_session_id, next.challenge.request_id);
    });
  }

  function openRecovery(request: VerificationRequest) {
    if (request.kind === 'join' || !isCurrentFlow(request)) return;
    void perform(() => openKeySyncRecovery(account_session_id, request.challenge.request_id));
  }

  if (status?.state === 'signed_out') return null;
  const button_disabled = disabled || busy || resolving;
  const verification_action =
    !status_error && status?.state === 'unverified' ? status.action : null;
  const check_error =
    status_error ||
    (status?.state === 'unverified' && status.action === 'check' ? status.error : '');
  const can_retry =
    !status_error &&
    status?.state === 'verified' &&
    ['failed', 'waiting_for_model_settings', 'conflict'].includes(status.sync_state);
  const can_review =
    !status_error && status?.state === 'verified' && status.sync_state === 'conflict';

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
            <strong>Verified</strong>
            {!status_error && <span role="status"> · {sync_labels[status.sync_state]}</span>}
          </p>
          {!status_error && status.sync_state === 'waiting_for_model_settings' && (
            <ModelSettingsRecovery
              enabled={model_sync_enabled}
              review_needed={model_review_needed}
            />
          )}
          {!status_error &&
            status.sync_state !== 'waiting_for_model_settings' &&
            status.sync_error && (
              <p className="status error" role="alert">
                {status.sync_error}
              </p>
            )}
          {!status_error && (
            <div className="account-sync-actions">
              {can_review && (
                <button
                  type="button"
                  className="primary"
                  disabled={button_disabled || checking || retrying}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true });
                    setError('');
                    showFlow({ kind: 'review' });
                  }}
                >
                  Review key conflicts
                </button>
              )}
              {can_retry && (
                <button
                  type="button"
                  disabled={button_disabled || checking || retrying}
                  onClick={() => void retry()}
                >
                  {retrying ? 'Retrying…' : 'Retry key sync'}
                </button>
              )}
              <button
                type="button"
                disabled={button_disabled}
                onClick={() => {
                  setError('');
                  showFlow({ kind: 'approve' });
                }}
              >
                Approve a device
              </button>
            </div>
          )}
          {can_retry && retry_error && (
            <p className="status error" role="alert">
              {retry_error}
            </p>
          )}
        </>
      )}
      {status?.state === 'unverified' && (
        <>
          <p>
            <strong>Unverified</strong>
          </p>
          {verification_action === 'setup' && (
            <>
              <p>
                Set up encrypted sync for this Account. Eligible API keys sync automatically after
                verification.
              </p>
              <button
                type="button"
                className="primary"
                disabled={button_disabled || checking}
                onClick={() =>
                  void perform(async (isCurrent) => {
                    await createKeySync(account_session_id);
                    if (isCurrent()) deviceVerified();
                  })
                }
              >
                Verify this device
              </button>
            </>
          )}
          {verification_action === 'pair' && (
            <>
              <div className="key-sync-option">
                <p>Approve this device from another device that is already verified.</p>
                <button
                  type="button"
                  className="primary"
                  disabled={button_disabled || checking}
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
                  disabled={button_disabled || checking}
                  onClick={() => void startRequest('recover')}
                >
                  Recover access
                </button>
              </div>
            </>
          )}
          {verification_action === 'upgrade' && (
            <>
              <p>Verify your Account to keep using your existing encrypted keys.</p>
              <button
                type="button"
                className="primary"
                disabled={button_disabled || checking}
                onClick={() => void startRequest('upgrade')}
              >
                Verify Account
              </button>
            </>
          )}
          {verification_action === 'legacy' && (
            <p>
              Open Apell on a device that already has your keys to finish setting up Account
              recovery.
            </p>
          )}
        </>
      )}
      {check_error && (
        <>
          <p className="status error" role="alert">
            {check_error}
          </p>
          <button type="button" disabled={busy || checking} onClick={() => void refresh()}>
            {checking ? 'Checking…' : 'Check again'}
          </button>
        </>
      )}
      {!flow && error && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
      {flow && (
        <Sheet
          open
          title={titles[flow.kind]}
          onClose={close}
          dismissible={!busy && !approving && !resolving}
        >
          {flow.kind === 'review' ? (
            <KeySyncConflictReview
              account_session_id={account_session_id}
              providers={providers}
              disabled={button_disabled || status?.state !== 'verified'}
              onMutationChange={(active) => {
                resolution_pending.current = active;
                setResolving(active);
                onBusyChange(active);
              }}
              onApplied={() => {
                onApplied();
                void refresh();
              }}
            />
          ) : (
            <div className="transfer-form">
              {flow.kind === 'approve' ? (
                <Approvals
                  account_session_id={account_session_id}
                  request={flow}
                  isCurrent={isCurrentFlow}
                  onBusyChange={(active) => {
                    setApproving(active);
                    onBusyChange(active);
                  }}
                />
              ) : (
                <Verification
                  account_session_id={account_session_id}
                  request={flow}
                  isCurrent={isCurrentFlow}
                  onComplete={deviceVerified}
                  busy={busy}
                  onOpen={() => openRecovery(flow)}
                  onRestart={() => void startRequest(flow.kind, flow)}
                />
              )}
              {error && (
                <p className="status error" role="alert">
                  {error}
                </p>
              )}
            </div>
          )}
        </Sheet>
      )}
    </section>
  );
}
