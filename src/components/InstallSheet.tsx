import { useEffect, useRef, useState } from 'react';
import type { GitHubRelease, InstalledApplet } from '../lib/types';
import { githubRelease, message } from '../lib/tauri';
import { Icon } from './Icon';
import { Sheet } from './Sheet';

interface InstallSheetProps {
  open: boolean;
  busy: boolean;
  hasSample: boolean;
  applets: InstalledApplet[];
  status: string;
  failed: boolean;
  onClose: () => void;
  onSample: () => void;
  onFile: (file: File) => void;
  onGitHub: (release: GitHubRelease, asset_name: string) => void;
  onUpdate: (applet_id: string) => void;
}

export function InstallSheet(props: InstallSheetProps) {
  const [repository, setRepository] = useState('');
  const [release, setRelease] = useState<GitHubRelease>();
  const [asset_name, setAssetName] = useState('');
  const [finding, setFinding] = useState(false);
  const [lookup_error, setLookupError] = useState('');
  const lookup = useRef(0);
  useEffect(
    () => () => {
      lookup.current++;
    },
    [],
  );
  useEffect(() => {
    if (!props.open) {
      lookup.current++;
      setFinding(false);
    }
  }, [props.open]);
  const disabled = props.busy || finding;
  return (
    <Sheet open={props.open} title="Add an applet" onClose={props.onClose}>
      <button className="install-option" disabled={disabled} onClick={props.onSample}>
        <span className="option-icon">
          <Icon name="chat" />
        </span>
        <span>
          <strong>{props.hasSample ? 'Update Companion' : 'Try Companion'}</strong>
          <small>A chatbot with a native WASM backend</small>
        </span>
        <Icon name="add" />
      </button>
      <label className={`install-option file-option${disabled ? ' disabled' : ''}`}>
        <span className="option-icon">
          <Icon name="file" />
        </span>
        <span>
          <strong>Install an .apel file</strong>
          <small>Choose a package from this device</small>
        </span>
        <input
          type="file"
          accept=".apel"
          aria-label="Choose an .apel file"
          disabled={disabled}
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = '';
            if (file) props.onFile(file);
          }}
        />
      </label>
      <details className="url-install">
        <summary>Install from GitHub</summary>
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            const attempt = ++lookup.current;
            setFinding(true);
            setLookupError('');
            setRelease(undefined);
            try {
              const found = await githubRelease(repository.trim());
              if (lookup.current !== attempt) return;
              setRelease(found);
              setAssetName(found.assets.length === 1 ? found.assets[0].name : '');
            } catch (error) {
              if (lookup.current === attempt) setLookupError(message(error));
            } finally {
              if (lookup.current === attempt) setFinding(false);
            }
          }}
        >
          <label>
            Public repository
            <input
              required
              value={repository}
              placeholder="owner/repository"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              disabled={disabled}
              onChange={(event) => {
                setRepository(event.target.value);
                setRelease(undefined);
                setLookupError('');
              }}
            />
          </label>
          <button type="submit" disabled={disabled}>
            {finding ? 'Finding release…' : 'Find release'}
          </button>
        </form>
        {release && (
          <div className="provider-form">
            <p>
              {release.repository} · {release.tag}
            </p>
            <label>
              Package
              <select
                value={asset_name}
                disabled={props.busy}
                onChange={(event) => setAssetName(event.target.value)}
              >
                <option value="" disabled>
                  Choose an .apel package
                </option>
                {release.assets.map((asset) => (
                  <option key={asset.name} value={asset.name}>
                    {asset.name} ({Math.round(asset.size_bytes / 1024)} KiB)
                  </option>
                ))}
              </select>
            </label>
            <button
              type="button"
              className="primary"
              disabled={props.busy || !asset_name}
              onClick={() => props.onGitHub(release, asset_name)}
            >
              {props.busy ? 'Installing…' : 'Download & install'}
            </button>
          </div>
        )}
        {lookup_error && (
          <p className="status error" role="alert">
            {lookup_error}
          </p>
        )}
      </details>
      {props.applets.some((applet) => applet.source) && (
        <div className="connection-list">
          <h3>GitHub updates</h3>
          {props.applets
            .filter((applet) => applet.source)
            .map((applet) => (
              <div className="connection-row" key={applet.applet_id}>
                <span>
                  <strong>{applet.name}</strong>
                  <small>
                    {applet.version} · {applet.source!.repository}
                  </small>
                </span>
                <button
                  type="button"
                  disabled={disabled}
                  onClick={() => props.onUpdate(applet.applet_id)}
                >
                  Update
                </button>
              </div>
            ))}
        </div>
      )}
      {props.status && (
        <p
          className={`status${props.failed ? ' error' : ''}`}
          role={props.failed ? 'alert' : 'status'}
        >
          {props.status}
        </p>
      )}
    </Sheet>
  );
}
