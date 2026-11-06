import { useEffect, useRef, useState } from 'react';
import { forgetMicrophone, listAppletPermissions, message } from '../lib/tauri';
import type { AppletPermission } from '../lib/types';
import { OperationScope } from './providers/state';

const microphoneLabels: Record<AppletPermission['microphone_access'], string> = {
  ask: 'Ask before recording',
  remembered: 'Microphone approval saved',
  previous_package: 'Approval saved for an earlier package',
};

export function AppletPermissions() {
  const [applets, setApplets] = useState<AppletPermission[]>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());

  const load = async (applet_id?: string) => {
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(true);
    setError('');
    try {
      if (applet_id) await forgetMicrophone(applet_id);
      if (!operation.isCurrent()) return;
      const entries = await listAppletPermissions();
      if (operation.isCurrent()) setApplets(entries);
    } catch (error) {
      if (operation.isCurrent()) setError(message(error));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setBusy(false);
      }
    }
  };

  useEffect(() => {
    void load();
    return () => scope.current.cancel();
  }, []);

  return (
    <section className="panel applet-permissions" aria-labelledby="applet-permissions-title">
      <h2 id="applet-permissions-title">Applet permissions</h2>
      <p>Manage saved microphone access. Resetting a grant makes the applet ask again.</p>
      {applets?.length ? (
        <div className="connection-list" aria-busy={busy}>
          {applets.map((applet) => (
            <div className="connection-row" key={applet.applet_id}>
              <div className="connection-detail">
                <strong>{applet.name}</strong>
                <small>{microphoneLabels[applet.microphone_access]}</small>
              </div>
              {applet.microphone_access !== 'ask' && (
                <button
                  type="button"
                  className="connection-use"
                  aria-label={`Reset microphone access for ${applet.name}`}
                  disabled={busy}
                  onClick={() => void load(applet.applet_id)}
                >
                  Reset
                </button>
              )}
            </div>
          ))}
        </div>
      ) : (
        <p className="status" role="status">
          {busy ? 'Loading permissions…' : applets ? 'No installed applets use audio.' : null}
        </p>
      )}
      {error && (
        <>
          <p className="status error" role="alert">
            {error}
          </p>
          <button type="button" onClick={() => void load()}>
            Try again
          </button>
        </>
      )}
    </section>
  );
}
