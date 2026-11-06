import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { getMicrophoneMode, message, setMicrophoneMode } from '../lib/tauri';
import type { AppletSession, MicrophoneMode } from '../lib/types';
import { Icon } from './Icon';
import { Sheet } from './Sheet';
import { OperationScope } from './providers/state';

const permission_labels: Record<string, string> = {
  sqlite: 'Saved app data',
  llm: 'Shared AI',
  resources: 'Downloaded resources',
  device: 'Device information',
  http: 'Network requests',
  fs: 'App files',
  config: 'App preferences',
  websocket: 'Live connections',
  secrets: 'Secure storage',
  crypto: 'Secure randomness',
  runtime: 'Timers',
  audio: 'Audio files and playback',
  speech: 'Speech transcription',
};

const offline_status: Record<string, string> = {
  http: 'Off',
  websocket: 'Off',
  llm: 'Local only',
  resources: 'Local only',
  speech: 'Installed model only',
};

const microphone_labels: Record<MicrophoneMode, string> = {
  ask: 'Ask every time',
  remembered: 'Always allow',
};

function MicrophoneAccess({ session, read_only }: { session: AppletSession; read_only: boolean }) {
  const { session_id, view_id } = session;
  const label_id = useId();
  const [mode, setMode] = useState<MicrophoneMode | 'unavailable'>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());

  const update = useCallback(
    async (next?: MicrophoneMode) => {
      if (read_only) return;
      const operation = scope.current.start();
      if (!operation) return;
      setBusy(true);
      setError('');
      try {
        const saved =
          next === undefined
            ? await getMicrophoneMode(session_id, view_id)
            : await setMicrophoneMode(session_id, view_id, next);
        if (operation.isCurrent()) setMode(saved);
      } catch (error) {
        if (operation.isCurrent()) setError(message(error));
      } finally {
        if (operation.isCurrent()) {
          operation.finish();
          setBusy(false);
        }
      }
    },
    [read_only, session_id, view_id],
  );

  useEffect(() => {
    const operations = scope.current;
    void update();
    return () => operations.cancel();
  }, [update]);

  return (
    <div>
      <dt id={label_id}>Microphone</dt>
      <dd>
        {read_only || mode === 'unavailable' ? (
          mode && mode !== 'unavailable' ? (
            microphone_labels[mode]
          ) : (
            'Unavailable'
          )
        ) : mode ? (
          <select
            aria-labelledby={label_id}
            value={mode}
            disabled={busy}
            onChange={(event) => {
              const value = event.currentTarget.value;
              if (value === 'ask' || value === 'remembered') void update(value);
            }}
          >
            <option value="ask">{microphone_labels.ask}</option>
            <option value="remembered">{microphone_labels.remembered}</option>
          </select>
        ) : busy ? (
          <span role="status">Loading…</span>
        ) : (
          <button type="button" onClick={() => void update()}>
            Try again
          </button>
        )}
      </dd>
      {!read_only && error && (
        <dd className="applet-access-error status error" role="alert">
          {error}
        </dd>
      )}
    </div>
  );
}

interface AppletControlsProps {
  session: AppletSession;
  disabled: boolean;
  stopped?: boolean;
  onClose: () => void;
  onReload: () => void;
}

export function AppletControls({
  session,
  disabled,
  stopped,
  onClose,
  onReload,
}: AppletControlsProps) {
  const { manifest } = session;
  const [panel, setPanel] = useState<'menu' | 'about' | null>(null);
  return (
    <>
      <div className="applet-controls" role="group" aria-label="Applet controls">
        <button
          className="icon-button"
          aria-label="More applet actions"
          title="More applet actions"
          aria-haspopup="dialog"
          aria-expanded={panel !== null}
          disabled={disabled}
          onClick={(event) => {
            // Safari does not focus tapped buttons; give the sheet a stable return target.
            event.currentTarget.focus({ preventScroll: true });
            setPanel('menu');
          }}
        >
          <Icon name="more" />
        </button>
        <span className="applet-control-divider" aria-hidden="true" />
        <button
          className="icon-button"
          aria-label="Close applet"
          title="Close applet"
          disabled={disabled}
          onClick={onClose}
        >
          <Icon name="exit" />
        </button>
      </div>
      {panel && (
        <Sheet
          key={panel}
          open
          title={panel === 'about' ? 'About this applet' : manifest.name}
          onClose={() => setPanel(null)}
        >
          {panel === 'menu' ? (
            <div className="applet-menu-actions">
              <button
                className="applet-menu-action"
                disabled={disabled || stopped}
                onClick={() => {
                  setPanel(null);
                  onReload();
                }}
              >
                <Icon name="reload" />
                Reload applet
              </button>
              <button className="applet-menu-action" onClick={() => setPanel('about')}>
                <Icon name="info" />
                About this applet
              </button>
            </div>
          ) : (
            <div className="applet-about">
              <h3>{manifest.name}</h3>
              <p>Version {manifest.version}</p>
              <h4>Permissions</h4>
              <dl className="applet-access">
                <div>
                  <dt>Network</dt>
                  <dd>{manifest.network === false ? 'Off' : 'On'}</dd>
                </div>
                {manifest.permissions.map((permission) => (
                  <div key={permission}>
                    <dt>{permission_labels[permission] ?? permission}</dt>
                    <dd>
                      {manifest.network === false ? (offline_status[permission] ?? 'On') : 'On'}
                    </dd>
                  </div>
                ))}
                {manifest.permissions.includes('audio') && (
                  <MicrophoneAccess
                    key={session.view_id}
                    session={session}
                    read_only={disabled || Boolean(stopped)}
                  />
                )}
              </dl>
            </div>
          )}
        </Sheet>
      )}
    </>
  );
}
