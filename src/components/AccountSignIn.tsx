import { useEffect, useRef, useState } from 'react';
import type { AccountDeviceCode, AccountSignInPhase } from '../lib/types';
import { Sheet } from './Sheet';

interface AccountSignInProps {
  challenge: AccountDeviceCode | null;
  phase: AccountSignInPhase | 'failed' | 'cancelling';
  busy: boolean;
  error: string | null | undefined;
  check_failed: boolean;
  onCheck: () => void;
  onRetry: () => void;
  onOpen: () => void;
  onCancel: () => void;
}

const progress_text = {
  requesting_code: 'Requesting a sign-in code…',
  waiting_for_approval: 'Checking sign-in status…',
  finishing: 'Checking sign-in status…',
  cancelling: 'Cancelling sign-in…',
};

function SignInCode({
  challenge,
  busy,
  onOpen,
  onExpired,
}: {
  challenge: AccountDeviceCode;
  busy: boolean;
  onOpen: () => void;
  onExpired: () => void;
}) {
  const code = useRef<HTMLElement>(null);
  const [copy_status, setCopyStatus] = useState('');
  const expires_at = new Date(challenge.expires_at);

  const useCode = () => {
    if (Date.now() < expires_at.getTime()) return true;
    onExpired();
    return false;
  };

  async function copyCode() {
    if (!useCode()) return;
    try {
      await navigator.clipboard.writeText(challenge.user_code);
      setCopyStatus('Code copied.');
    } catch {
      if (code.current) window.getSelection()?.selectAllChildren(code.current);
      setCopyStatus('Copy the selected code.');
    }
  }
  return (
    <>
      <div className="device-login">
        <p>Enter this code in a browser on this device or another device.</p>
        <code ref={code}>{challenge.user_code}</code>
        <p className="status">{challenge.verification_uri}</p>
        <p className="status">
          Code expires at{' '}
          <time dateTime={challenge.expires_at}>
            {expires_at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}
          </time>
          .
        </p>
      </div>
      <p className="status" role="status">
        Waiting for sign-in… {copy_status}
      </p>
      <div className="button-row">
        <button type="button" disabled={busy} onClick={() => void copyCode()}>
          Copy code
        </button>
        <button
          className="primary"
          type="button"
          disabled={busy}
          onClick={() => {
            if (useCode()) onOpen();
          }}
        >
          Open sign-in page
        </button>
      </div>
    </>
  );
}

export function AccountSignIn({
  challenge,
  phase,
  busy,
  error,
  check_failed,
  onCheck,
  onRetry,
  onOpen,
  onCancel,
}: AccountSignInProps) {
  const deadline = challenge ? Date.parse(challenge.expires_at) : undefined;
  const [elapsed_deadline, setElapsedDeadline] = useState<number>();
  const status = useRef<HTMLParagraphElement>(null);
  const retry = useRef<HTMLButtonElement>(null);
  const check = useRef(onCheck);
  check.current = onCheck;
  const expire = () => {
    setElapsedDeadline(deadline);
    check.current();
  };
  useEffect(() => {
    if (deadline === undefined || !Number.isFinite(deadline) || phase !== 'waiting_for_approval')
      return;
    let timer: number | undefined;
    const update = () => {
      window.clearTimeout(timer);
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        setElapsedDeadline(deadline);
        return;
      }
      timer = window.setTimeout(() => {
        update();
        if (Date.now() >= deadline) check.current();
      }, remaining);
    };
    update();
    window.addEventListener('focus', update);
    document.addEventListener('visibilitychange', update);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener('focus', update);
      document.removeEventListener('visibilitychange', update);
    };
  }, [deadline, phase]);
  const active_code =
    phase === 'waiting_for_approval' &&
    challenge &&
    deadline !== undefined &&
    deadline !== elapsed_deadline &&
    Date.now() < deadline;
  const previous = useRef({ phase, had_code: Boolean(active_code) });
  useEffect(() => {
    const changed = previous.current.phase !== phase || (previous.current.had_code && !active_code);
    previous.current = { phase, had_code: Boolean(active_code) };
    const focused = document.activeElement;
    if (
      changed &&
      (focused === document.body || focused === status.current?.closest('[role="dialog"]'))
    ) {
      const target = phase === 'failed' && !busy ? retry.current : status.current;
      target?.focus({ preventScroll: true });
    }
  }, [active_code, busy, phase]);

  return (
    <Sheet open title="Sign in to Apell" onClose={onCancel} dismissible={!busy}>
      <div className="transfer-form">
        {active_code ? (
          <SignInCode
            key={`${challenge.user_code}:${challenge.expires_at}`}
            challenge={challenge}
            busy={busy}
            onOpen={onOpen}
            onExpired={expire}
          />
        ) : (
          phase !== 'failed' && (
            <p ref={status} tabIndex={-1} className="status" role="status">
              {progress_text[phase]}
            </p>
          )
        )}
        {(error || phase === 'failed') && (
          <p
            ref={phase === 'failed' ? status : undefined}
            tabIndex={phase === 'failed' ? -1 : undefined}
            className="status error"
            role="alert"
          >
            {error || 'Sign-in ended. Get a new code to try again.'}
          </p>
        )}
        <div className="button-row">
          <button type="button" disabled={busy} onClick={onCancel}>
            {phase === 'failed' ? 'Back to Account' : 'Cancel sign-in'}
          </button>
          {phase === 'failed' ? (
            <button ref={retry} className="primary" type="button" disabled={busy} onClick={onRetry}>
              Get a new code
            </button>
          ) : (
            check_failed && (
              <button type="button" disabled={busy} onClick={onCheck}>
                Check sign-in status
              </button>
            )
          )}
        </div>
      </div>
    </Sheet>
  );
}
