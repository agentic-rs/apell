import { useEffect, useRef, useState } from 'react';
import { managed_permission_labels } from '../lib/permissions';
import { forgetPermission, listAppletPermissions, message } from '../lib/tauri';
import type { AppletPermission, ManagedPermission, PermissionGrant } from '../lib/types';
import { OperationScope } from './providers/state';

const access_labels: Record<PermissionGrant['access'], string> = {
  ask: 'Ask every time',
  remembered: 'Always allow',
  previous_package: 'Approval saved for an earlier package',
};

export function AppletPermissions() {
  const [applets, setApplets] = useState<AppletPermission[]>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());

  const load = async (reset?: { applet_id: string; permission: ManagedPermission }) => {
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(true);
    setError('');
    try {
      if (reset) await forgetPermission(reset.applet_id, reset.permission);
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
      <p>Reset saved microphone, camera, or location access to make an applet ask again.</p>
      {applets?.length ? (
        <div className="connection-list" aria-busy={busy}>
          {applets.map((applet) => (
            <div className="connection-row" key={applet.applet_id}>
              <strong>{applet.name}</strong>
              {applet.grants.map(({ permission, access }) => (
                <div className="applet-grant" key={permission}>
                  <div className="connection-detail">
                    <span>{managed_permission_labels[permission]}</span>
                    <small>{access_labels[access]}</small>
                  </div>
                  {access !== 'ask' && (
                    <button
                      type="button"
                      className="connection-use"
                      aria-label={`Reset ${permission} access for ${applet.name}`}
                      disabled={busy}
                      onClick={() => void load({ applet_id: applet.applet_id, permission })}
                    >
                      Reset
                    </button>
                  )}
                </div>
              ))}
            </div>
          ))}
        </div>
      ) : (
        <p className="status" role="status">
          {busy ? 'Loading permissions…' : applets ? 'No applets with managed permissions.' : null}
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
