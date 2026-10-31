import { useState } from 'react';
import { Icon } from './Icon';
import { Sheet } from './Sheet';

interface InstallSheetProps {
  open: boolean;
  busy: boolean;
  hasSample: boolean;
  status: string;
  failed: boolean;
  onClose: () => void;
  onSample: () => void;
  onFile: (file: File) => void;
  onUrl: (url: string, sha256: string) => void;
}

export function InstallSheet({
  open,
  busy,
  hasSample,
  status,
  failed,
  onClose,
  onSample,
  onFile,
  onUrl,
}: InstallSheetProps) {
  const [url, setUrl] = useState('');
  const [sha256, setSha256] = useState('');

  return (
    <Sheet open={open} title="Add an applet" onClose={onClose}>
      <button className="install-option" disabled={busy} onClick={onSample}>
        <span className="option-icon">
          <Icon name="chat" />
        </span>
        <span>
          <strong>{hasSample ? 'Update Companion' : 'Try Companion'}</strong>
          <small>
            {hasSample ? 'Refresh the sample app and keep your chats' : 'A sample chatbot'}
          </small>
        </span>
        <Icon name="add" />
      </button>
      <label className={`install-option file-option${busy ? ' disabled' : ''}`}>
        <span className="option-icon">
          <Icon name="file" />
        </span>
        <span>
          <strong>Choose a bundle</strong>
          <small>From Files on this device</small>
        </span>
        <input
          type="file"
          accept=".json,.apell"
          aria-label="Choose a bundle"
          disabled={busy}
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = '';
            if (file) onFile(file);
          }}
        />
      </label>
      <details className="url-install">
        <summary>Install from a URL</summary>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            onUrl(url.trim(), sha256.trim());
          }}
        >
          <label>
            Bundle URL
            <input
              type="url"
              required
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              placeholder="https://example.com/app.apell.json"
              value={url}
              disabled={busy}
              onChange={(event) => setUrl(event.target.value)}
            />
          </label>
          <label>
            SHA-256 checksum
            <input
              required
              pattern="[a-fA-F0-9]{64}"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              placeholder="64-character checksum"
              value={sha256}
              disabled={busy}
              onChange={(event) => setSha256(event.target.value)}
            />
          </label>
          <button className="primary" disabled={busy}>
            {busy ? 'Installing…' : 'Download & install'}
          </button>
        </form>
      </details>
      {status && (
        <p className={`status${failed ? ' error' : ''}`} role={failed ? 'alert' : 'status'}>
          {status}
        </p>
      )}
    </Sheet>
  );
}
