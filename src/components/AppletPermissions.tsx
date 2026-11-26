import { useEffect, useRef, useState } from 'react';
import { managed_permission_labels } from '../lib/permissions';
import { forgetPermission, listAppletPermissions, message } from '../lib/tauri';
import type { AppletPermission, ManagedPermission, PermissionGrant } from '../lib/types';
import { OperationScope } from './providers/state';
import './AppletPermissions.css';

const access_labels: Record<PermissionGrant['access'], string> = {
  ask: 'Ask every time',
  remembered: 'Always allow',
  previous_package: 'Approval saved for an earlier package',
  undeclared: 'No longer requested',
  unverified_package: 'Applet unavailable',
};

function PermissionRow({
  applet_name,
  grant,
  busy,
  reset,
}: {
  applet_name: string;
  grant: PermissionGrant;
  busy: boolean;
  reset: () => void;
}) {
  const { permission, access, available } = grant;
  const label = managed_permission_labels[permission];
  const platform_unavailable = !available && access !== 'unverified_package';
  const can_reset = access !== 'ask';
  return (
    <div className="applet-permission-row">
      <dt>{label}</dt>
      <dd>
        <div className="applet-permission-state">
          <span>
            {platform_unavailable ? 'Unavailable on this platform' : access_labels[access]}
          </span>
          {platform_unavailable && can_reset && (
            <small>{access === 'remembered' ? 'Approval saved' : access_labels[access]}</small>
          )}
        </div>
        {can_reset && (
          <button
            type="button"
            className="connection-use applet-permission-reset"
            aria-label={`Reset ${label} access for ${applet_name}`}
            disabled={busy}
            onClick={reset}
          >
            Reset
          </button>
        )}
      </dd>
    </div>
  );
}

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
    <section className="panel applet-permissions" aria-labelledby="settings-permissions">
      <h2 id="settings-permissions" className="settings-section-title" tabIndex={-1}>
        Applet permissions
      </h2>
      <p>Manage saved microphone, camera, and location approvals in Apell.</p>
      {applets?.length ? (
        <div className="applet-permission-groups" aria-busy={busy}>
          {applets.map((applet) => (
            <div className="applet-permission-group" key={applet.applet_id}>
              <h3>{applet.name}</h3>
              <dl>
                {applet.grants.map((grant) => (
                  <PermissionRow
                    key={grant.permission}
                    applet_name={applet.name}
                    grant={grant}
                    busy={busy}
                    reset={() =>
                      void load({ applet_id: applet.applet_id, permission: grant.permission })
                    }
                  />
                ))}
              </dl>
            </div>
          ))}
        </div>
      ) : (
        (applets || busy) && (
          <p className="status" role="status">
            {applets ? 'No applets with managed permissions.' : 'Loading permissions…'}
          </p>
        )
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
