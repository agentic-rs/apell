import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  approveKeySyncPairing,
  cancelKeySyncPairing,
  cancelKeySyncRecovery,
  createKeySync,
  finishKeySyncPairing,
  finishKeySyncRecovery,
  getKeySyncStatus,
  getLocalKeySyncStatus,
  listKeySyncPairings,
  message,
  onKeySyncChanged,
  openKeySyncRecovery,
  retryKeySync,
  startKeySyncPairing,
  startKeySyncRecovery,
} from '../lib/tauri';
import type {
  KeySyncPairing,
  KeySyncRecovery,
  KeySyncStatus,
  LlmConnection,
  LlmProvider,
  LocalKeySyncStatus,
} from '../lib/types';
import { KeySyncConflictReview } from './KeySyncConflictReview';
import { OperationScope } from './providers/state';
import { Sheet } from './Sheet';

const poll_interval = 3_000;
const expired_request_error = 'This request expired. Start again.';
const changed_vault_error = 'The saved encryption vault changed. Start verification again.';
const expired_approval_error =
  'This pairing code expired. Ask the other device to show a new code.';
const missing_approval_error =
  'This pairing request is no longer available. Check the other device.';

type VerificationRequest =
  | { kind: 'join'; pairing: KeySyncPairing }
  | { kind: 'recover' | 'upgrade'; challenge: KeySyncRecovery };
type VerificationStartup = {
  kind: 'startup';
  verification_kind: VerificationRequest['kind'];
  error?: string;
};
type Flow = VerificationRequest | VerificationStartup | { kind: 'approve' } | { kind: 'review' };
type Progress = 'waiting' | 'expired' | 'rejected' | 'vault_changed' | { error: string };

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

function VerificationPreparation({
  startup,
  pending,
  onCancel,
  onRetry,
}: {
  startup: VerificationStartup;
  pending: boolean;
  onCancel: () => void;
  onRetry: () => void;
}) {
  return (
    <>
      {startup.error ? (
        <p className="status error" role="alert">
          {startup.error}
        </p>
      ) : (
        <p role="status">
          {startup.verification_kind === 'join'
            ? 'Requesting a pairing code…'
            : 'Preparing Account verification…'}
        </p>
      )}
      <div className="button-row">
        <button type="button" onClick={onCancel}>
          {startup.error ? 'Back to Account' : 'Cancel'}
        </button>
        {startup.error && (
          <button type="button" className="primary" disabled={pending} onClick={onRetry}>
            Try again
          </button>
        )}
      </div>
    </>
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
      if (text === changed_vault_error) {
        onProgress('vault_changed');
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
  const restart_feedback =
    progress === 'rejected'
      ? 'Account could not verify this request.'
      : progress === 'vault_changed'
        ? changed_vault_error
        : progress === 'expired'
          ? pairing
            ? 'This pairing code expired.'
            : 'Account verification expired.'
          : undefined;
  if (restart_feedback) {
    return (
      <>
        <p role="alert" className="status error">
          {restart_feedback}
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
  operation_gate,
  onBusyChange,
}: {
  account_session_id: string;
  request: Extract<Flow, { kind: 'approve' }>;
  isCurrent: (request: Flow) => boolean;
  operation_gate: OperationScope;
  onBusyChange: (busy: boolean) => void;
}) {
  const [pairs, setPairs] = useState<KeySyncPairing[]>();
  const reviewed = useRef<KeySyncPairing[] | undefined>(undefined);
  const [checking, setChecking] = useState(true);
  const [poll_error, setPollError] = useState('');
  const [approval_error, setApprovalError] = useState<{ pair: KeySyncPairing; text: string }>();
  const [busy, setBusy] = useState<string>();
  const [outcome, setOutcome] = useState<{ text: string; error?: boolean }>();
  const presentation = useRef<symbol | undefined>(undefined);
  const dismissed = useRef(new Set<string>());
  const instructions = useRef<HTMLParagraphElement>(null);
  const check_again = useRef<HTMLButtonElement>(null);
  const focused_control = useRef<HTMLButtonElement>(null);

  const publishPairs = useCallback((next?: KeySyncPairing[]) => {
    reviewed.current = next;
    setPairs(next);
  }, []);

  useLayoutEffect(() => {
    presentation.current = Symbol();
    reviewed.current = undefined;
    setPairs(undefined);
    setChecking(true);
    setPollError('');
    setApprovalError(undefined);
    setBusy(undefined);
    setOutcome(undefined);
    return () => {
      presentation.current = undefined;
      reviewed.current = undefined;
    };
  }, [account_session_id, request, isCurrent]);

  const removeExpired = useCallback(() => {
    const current = reviewed.current;
    if (current?.some((pair) => hasExpired(pair.expires_at))) {
      publishPairs(current.filter((pair) => !hasExpired(pair.expires_at)));
    }
  }, [publishPairs]);

  const checkRequests = useCallback(async () => {
    const opening = presentation.current;
    if (!opening || !isCurrent(request)) return;
    removeExpired();
    const operation = operation_gate.start();
    if (!operation) return;
    const isOpen = () => presentation.current === opening && isCurrent(request);
    setChecking(true);
    try {
      const result = await listKeySyncPairings(account_session_id);
      if (isOpen()) {
        publishPairs(
          result.pairs.filter(
            (pair) =>
              !pair.approved &&
              !hasExpired(pair.expires_at) &&
              !dismissed.current.has(pairKey(pair)),
          ),
        );
        setPollError('');
      }
    } catch (cause) {
      if (isOpen()) {
        publishPairs();
        setPollError(message(cause) || 'Try again to check for pairing requests.');
      }
    } finally {
      operation.finish();
      if (isOpen()) setChecking(false);
    }
  }, [account_session_id, request, isCurrent, operation_gate, publishPairs, removeExpired]);

  useEffect(() => {
    const refresh = () => {
      if (!document.hidden) void checkRequests();
    };
    refresh();
    const timer = window.setInterval(refresh, poll_interval);
    window.addEventListener('focus', refresh);
    document.addEventListener('visibilitychange', refresh);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', refresh);
      document.removeEventListener('visibilitychange', refresh);
    };
  }, [checkRequests]);

  useEffect(() => {
    if (!pairs?.length) return;
    const deadline = Math.min(...pairs.map((pair) => pair.expires_at * 1000));
    const timer = window.setTimeout(removeExpired, Math.max(0, deadline - Date.now()));
    return () => window.clearTimeout(timer);
  }, [pairs, removeExpired]);

  useLayoutEffect(() => {
    const previous = focused_control.current;
    if (!previous) return;
    if (!previous.isConnected) focused_control.current = null;
    const focused = document.activeElement;
    if (focused !== document.body && focused !== instructions.current) return;
    const target = previous.isConnected ? previous : check_again.current;
    (target && !target.disabled ? target : instructions.current)?.focus({ preventScroll: true });
  }, [pairs, poll_error, checking, busy]);

  async function approve(pair: KeySyncPairing) {
    const opening = presentation.current;
    if (!opening || !isCurrent(request) || document.hidden) return;
    const key = pairKey(pair);
    const current = reviewed.current?.find((review) => pairKey(review) === key);
    if (!current) return;
    if (hasExpired(current.expires_at)) {
      removeExpired();
      setOutcome({ text: expired_approval_error, error: true });
      return;
    }
    const operation = operation_gate.start();
    if (!operation) return;
    const isOpen = () => presentation.current === opening && isCurrent(request);
    const dismiss = () => {
      dismissed.current.add(key);
      publishPairs(reviewed.current?.filter((pair) => pairKey(pair) !== key));
    };
    setBusy(key);
    onBusyChange(true);
    setApprovalError(undefined);
    setOutcome(undefined);
    try {
      await approveKeySyncPairing(account_session_id, current.pair_id, current.approval_nonce);
      if (isOpen()) {
        dismiss();
        setOutcome({ text: 'Device approved. It will finish verification.' });
      }
    } catch (cause) {
      if (isOpen()) {
        const text = message(cause) || 'Could not confirm approval. Check the other device.';
        if (text === expired_request_error) {
          dismiss();
          setOutcome({
            text: hasExpired(current.expires_at) ? expired_approval_error : missing_approval_error,
            error: true,
          });
        } else if (hasExpired(current.expires_at)) {
          setOutcome({ text: 'Could not confirm approval. Check the other device.', error: true });
        } else {
          setApprovalError({ pair: current, text });
        }
      }
    } finally {
      operation.finish();
      onBusyChange(false);
      if (isOpen()) {
        setBusy(undefined);
        if (!document.hidden) void checkRequests();
      }
    }
  }

  const checking_approval = busy && !pairs?.some((pair) => pairKey(pair) === busy);
  const unconfirmed_approval =
    approval_error && !pairs?.some((pair) => pairKey(pair) === pairKey(approval_error.pair));
  return (
    <>
      <p ref={instructions} tabIndex={-1}>
        In <strong>Settings → Account</strong> on the new device, choose{' '}
        <strong>Pair this device</strong>. Approve only when the six digits match.
      </p>
      {checking_approval && <p role="status">Checking approval…</p>}
      {!pairs && !poll_error && <p role="status">Checking for devices…</p>}
      {pairs?.length === 0 && !outcome && !checking_approval && !unconfirmed_approval && (
        <p role="status">Waiting for a pairing request…</p>
      )}
      {pairs?.map((pair) => {
        const key = pairKey(pair);
        const error = approval_error && key === pairKey(approval_error.pair) && approval_error.text;
        const error_id = `pair-approval-error-${pair.pair_id}`;
        return (
          <div className="key-sync-approval" key={key}>
            <PairingCode code={pair.code} />
            <button
              type="button"
              className="primary"
              aria-label={`Approve code ${[...pair.code].join(' ')}`}
              aria-describedby={error ? error_id : undefined}
              disabled={checking || !!busy}
              onFocus={(event) => (focused_control.current = event.currentTarget)}
              onClick={(event) => {
                event.currentTarget.focus({ preventScroll: true });
                void approve(pair);
              }}
            >
              {busy === key ? 'Approving…' : 'Approve'}
            </button>
            {error && (
              <p id={error_id} role="alert" className="key-sync-approval-error status error">
                {error}
              </p>
            )}
          </div>
        );
      })}
      {outcome && (
        <p
          role={outcome.error ? 'alert' : 'status'}
          className={outcome.error ? 'status error' : undefined}
        >
          {outcome.text}
        </p>
      )}
      {unconfirmed_approval && (
        <p role="alert" className="status error">
          Could not confirm approval. Check the other device.
        </p>
      )}
      {poll_error && (
        <>
          <p role="alert" className="status error">
            Couldn’t check pairing requests. {poll_error}
          </p>
          <button
            ref={check_again}
            type="button"
            disabled={checking}
            onFocus={(event) => (focused_control.current = event.currentTarget)}
            onClick={(event) => {
              event.currentTarget.focus({ preventScroll: true });
              void checkRequests();
            }}
          >
            {checking ? 'Checking…' : 'Check again'}
          </button>
        </>
      )}
    </>
  );
}

function pairKey(pair: KeySyncPairing): string {
  return `${pair.pair_id}:${pair.approval_nonce}:${pair.code}`;
}

interface Props {
  account_session_id: string;
  providers: LlmProvider[];
  connections: LlmConnection[];
  disabled: boolean;
  model_sync_enabled?: boolean;
  model_review_needed: boolean;
  onApplied: () => void;
  onBusyChange: (busy: boolean) => void;
  onOpenConnections: (trigger: HTMLButtonElement, connection_id?: string) => void;
}

const sync_labels = {
  pending: 'Waiting to sync',
  syncing: 'Syncing keys…',
  current: 'Synced keys are up to date',
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
  connections,
  disabled,
  model_sync_enabled,
  model_review_needed,
  onApplied,
  onBusyChange,
  onOpenConnections,
}: Props) {
  const [status, setStatus] = useState<LocalKeySyncStatus | KeySyncStatus>();
  const [flow, setFlow] = useState<Flow>();
  const active_flow = useRef<Flow | undefined>(undefined);
  const [starting_verification, setStartingVerification] = useState(false);
  const start_pending = useRef(false);
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
  const approval_gate = useRef(new OperationScope());
  const read_version = useRef(0);
  const visible = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const coverage_focus = useRef<HTMLElement>(undefined);
  const verification_trigger = useRef<HTMLButtonElement>(null);
  const ending_verification = useRef<HTMLParagraphElement>(null);
  const verification_content = useRef<HTMLDivElement>(null);
  const outside_keys = status?.state === 'verified' ? status.keys_outside_account : null;
  const isCurrentFlow = useCallback((request: Flow) => active_flow.current === request, []);

  function showFlow(next?: Flow) {
    active_flow.current = next;
    setFlow(next);
  }

  function updateStatus(current: LocalKeySyncStatus | KeySyncStatus) {
    const active = active_flow.current?.kind;
    if (
      current.state === 'signed_out' ||
      ((active === 'review' || active === 'approve') && current.state !== 'verified')
    ) {
      showFlow();
    }
    setStatus(current);
    setStatusError('');
  }

  async function refresh() {
    if (!visible.current) return;
    const version = ++read_version.current;
    setChecking(true);
    try {
      const local = await getLocalKeySyncStatus(account_session_id);
      if (read_version.current !== version) return;
      updateStatus(local);
      if (local.state !== 'unverified') return;

      const current = await getKeySyncStatus(account_session_id);
      if (read_version.current === version) updateStatus(current);
    } catch (error) {
      if (read_version.current === version) setStatusError(message(error));
    } finally {
      if (read_version.current === version) setChecking(false);
    }
  }

  useLayoutEffect(() => {
    visible.current = true;
    return () => {
      visible.current = false;
      if (active_flow.current?.kind === 'startup') showFlow();
    };
  }, [account_session_id]);

  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
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
      window.removeEventListener('focus', reload);
      stop?.();
      read_version.current += 1;
      scope.current.cancel();
      retry_scope.current.cancel();
    };
  }, [account_session_id]);
  useLayoutEffect(() => {
    if (coverage_focus.current?.isConnected === false) {
      coverage_focus.current = undefined;
      if (document.activeElement === document.body) heading.current?.focus({ preventScroll: true });
    }
  }, [outside_keys, status_error]);
  useEffect(() => {
    if (flow) {
      if (document.activeElement === document.body)
        verification_content.current?.focus({ preventScroll: true });
      return;
    }
    const trigger = verification_trigger.current;
    if (!trigger) return;
    if (starting_verification) {
      if (document.activeElement === document.body)
        ending_verification.current?.focus({ preventScroll: true });
      return;
    }
    verification_trigger.current = null;
    if (document.activeElement === document.body) {
      (trigger.isConnected && !trigger.disabled ? trigger : heading.current)?.focus({
        preventScroll: true,
      });
    }
  }, [flow, starting_verification]);

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

  async function startRequest(
    verification_kind: VerificationRequest['kind'],
    replacing?: VerificationRequest | VerificationStartup,
  ) {
    if (start_pending.current || !visible.current || active_flow.current !== replacing) return;
    const startup: VerificationStartup = { kind: 'startup', verification_kind };
    const isCurrent = () => visible.current && isCurrentFlow(startup);
    start_pending.current = true;
    setStartingVerification(true);
    setError('');
    showFlow(startup);
    try {
      await cancelRequest(account_session_id, replacing);
      if (!isCurrent()) return;
      const next: VerificationRequest =
        verification_kind === 'join'
          ? { kind: verification_kind, pairing: await startKeySyncPairing(account_session_id) }
          : { kind: verification_kind, challenge: await startKeySyncRecovery(account_session_id) };
      if (!isCurrent()) {
        // Keep starts serialized until the abandoned challenge has been cancelled.
        await cancelRequest(account_session_id, next);
        return;
      }
      showFlow(next);
      if (next.kind !== 'join') openRecovery(next);
    } catch (cause) {
      if (isCurrent()) {
        showFlow({ ...startup, error: message(cause) || 'Couldn’t start verification.' });
        void refresh();
      }
    } finally {
      start_pending.current = false;
      setStartingVerification(false);
    }
  }

  function beginVerification(kind: VerificationRequest['kind'], trigger: HTMLButtonElement) {
    trigger.focus({ preventScroll: true });
    verification_trigger.current = trigger;
    void startRequest(kind);
  }

  function openRecovery(request: VerificationRequest) {
    if (request.kind === 'join' || !isCurrentFlow(request)) return;
    void perform(() => openKeySyncRecovery(account_session_id, request.challenge.request_id));
  }

  if (status?.state === 'signed_out') return null;
  const button_disabled = disabled || busy || resolving || starting_verification;
  const verification = status?.state === 'unverified' && 'action' in status ? status : undefined;
  const verification_action = !status_error && !checking ? verification?.action : undefined;
  const check_error = status_error || (verification?.action === 'check' ? verification.error : '');
  const can_retry =
    !status_error &&
    status?.state === 'verified' &&
    ['failed', 'waiting_for_model_settings', 'conflict'].includes(status.sync_state);
  const can_review =
    !status_error && status?.state === 'verified' && status.sync_state === 'conflict';
  const coverage_rows =
    outside_keys?.map(({ connection_id, name }) => {
      const connection = connections.find((item) => item.connection_id === connection_id);
      const provider = providers.find((item) => item.provider_id === connection?.provider_id);
      const detail = connection
        ? [provider?.name ?? connection.provider_id, connection.model_id]
            .filter(Boolean)
            .join(' · ')
        : '';
      return { connection_id, name, detail };
    }) ?? [];

  return (
    <section className="account-key-sync" aria-label="Encrypted API keys">
      <h3 ref={heading} tabIndex={-1}>
        Encrypted API keys
      </h3>
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
          {!status_error && coverage_rows.length > 0 && (
            <details
              className="key-sync-coverage"
              onFocus={(event) => {
                coverage_focus.current = event.target;
              }}
            >
              <summary>
                {coverage_rows.length} {coverage_rows.length === 1 ? 'API key' : 'API keys'} outside
                this Account’s sync
              </summary>
              <div className="key-sync-option">
                <p>
                  To sync a key with this Account, edit its connection, enter the key again, and
                  save.
                </p>
                <ul
                  className="key-sync-connections"
                  aria-label="Connections with keys outside sync"
                >
                  {coverage_rows.map(({ connection_id, name, detail }) => {
                    const duplicate = coverage_rows.some(
                      (other) =>
                        other.connection_id !== connection_id &&
                        other.name === name &&
                        other.detail === detail,
                    );
                    const description = [detail, duplicate ? connection_id : '']
                      .filter(Boolean)
                      .join(' · ');
                    return (
                      <li key={connection_id}>
                        <button
                          className="key-sync-connection"
                          type="button"
                          aria-label={
                            description ? `Edit ${name} (${description})` : `Edit ${name}`
                          }
                          disabled={button_disabled || retrying}
                          onClick={(event) => onOpenConnections(event.currentTarget, connection_id)}
                        >
                          <span className="key-sync-connection-identity">
                            <strong>
                              <bdi>{name}</bdi>
                            </strong>
                            {description && (
                              <small>
                                <bdi>{description}</bdi>
                              </small>
                            )}
                          </span>
                          <span aria-hidden="true">Edit</span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
                <button
                  type="button"
                  disabled={button_disabled || retrying}
                  onClick={(event) => onOpenConnections(event.currentTarget)}
                >
                  Open model connections
                </button>
              </div>
            </details>
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
                onClick={(event) => {
                  event.currentTarget.focus({ preventScroll: true });
                  verification_trigger.current = event.currentTarget;
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
          {status.local_issue === 'invalid_vault' && (
            <p>This device’s encryption vault is damaged. Apell keeps your local provider keys.</p>
          )}
          {checking && !check_error && <p role="status">Checking verification options…</p>}
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
                  onClick={(event) => beginVerification('join', event.currentTarget)}
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
                  onClick={(event) => beginVerification('recover', event.currentTarget)}
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
                onClick={(event) => beginVerification('upgrade', event.currentTarget)}
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
      {!flow && starting_verification && (
        <p ref={ending_verification} tabIndex={-1} role="status">
          Ending verification request…
        </p>
      )}
      {flow && (
        <Sheet
          open
          title={titles[flow.kind === 'startup' ? flow.verification_kind : flow.kind]}
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
            <div ref={verification_content} tabIndex={-1} className="transfer-form">
              {flow.kind === 'startup' ? (
                <VerificationPreparation
                  startup={flow}
                  pending={starting_verification}
                  onCancel={close}
                  onRetry={() => void startRequest(flow.verification_kind, flow)}
                />
              ) : flow.kind === 'approve' ? (
                <Approvals
                  account_session_id={account_session_id}
                  request={flow}
                  isCurrent={isCurrentFlow}
                  operation_gate={approval_gate.current}
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
