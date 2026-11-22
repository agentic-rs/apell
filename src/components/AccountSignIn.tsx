import { useRef, useState } from 'react';
import type { AccountDeviceCode } from '../lib/types';
import { Sheet } from './Sheet';

interface AccountSignInProps {
  challenge: AccountDeviceCode | null;
  busy: boolean;
  error: string | null | undefined;
  onOpen: () => void;
  onCancel: () => void;
}

export function AccountSignIn({ challenge, busy, error, onOpen, onCancel }: AccountSignInProps) {
  const code = useRef<HTMLElement>(null);
  const [copy_status, setCopyStatus] = useState('');
  const expires_at = challenge && new Date(challenge.expires_at);

  async function copyCode() {
    if (!challenge) return;
    try {
      await navigator.clipboard.writeText(challenge.user_code);
      setCopyStatus('Code copied.');
    } catch {
      if (code.current) window.getSelection()?.selectAllChildren(code.current);
      setCopyStatus('Copy the selected code.');
    }
  }

  return (
    <Sheet open title="Sign in to Apell" onClose={onCancel} dismissible={!busy}>
      <div className="transfer-form">
        {challenge ? (
          <>
            <div className="device-login">
              <p>Enter this code in a browser on this device or another device.</p>
              <code ref={code}>{challenge.user_code}</code>
              <p className="status">{challenge.verification_uri}</p>
              {expires_at && Number.isFinite(expires_at.getTime()) && (
                <p className="status">
                  Code expires at{' '}
                  <time dateTime={challenge.expires_at}>
                    {expires_at.toLocaleTimeString(undefined, {
                      hour: 'numeric',
                      minute: '2-digit',
                    })}
                  </time>
                  .
                </p>
              )}
            </div>
            <p className="status" role="status">
              Waiting for sign-in… {copy_status}
            </p>
            <div className="button-row">
              <button type="button" onClick={() => void copyCode()}>
                Copy code
              </button>
              <button className="primary" type="button" disabled={busy} onClick={onOpen}>
                Open sign-in page
              </button>
            </div>
          </>
        ) : (
          <p className="status" role="status">
            Requesting a sign-in code…
          </p>
        )}
        {error && (
          <p className="status error" role="alert">
            {error}
          </p>
        )}
        <button type="button" disabled={busy} onClick={onCancel}>
          Cancel sign-in
        </button>
      </div>
    </Sheet>
  );
}
