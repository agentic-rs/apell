import { useEffect, useRef, useState } from 'react';
import {
  appletPermissionRows,
  device_access_note,
  managed_permission_labels,
  permission_mode_labels,
  permission_unavailable,
  systemPermissionLabel,
} from '../lib/permissions';
import {
  forgetPermission,
  listAppletPermissions,
  message,
  onPermissionsChanged,
} from '../lib/tauri';
import type { AppletPermission, ManagedPermission, PermissionGrant } from '../lib/types';
import { OperationScope } from './providers/state';
import './AppletPermissions.css';

const access_labels: Record<PermissionGrant['access'], string> = {
  ...permission_mode_labels,
  previous_package: 'Approval saved for an earlier package',
  undeclared: 'Unused by installed package',
  unverified_package: 'Applet unavailable',
};

function PermissionRow({
  applet_name,
  grant,
  busy,
  unavailable_reason,
  reset,
}: {
  applet_name: string;
  grant: PermissionGrant;
  busy: boolean;
  unavailable_reason?: string;
  reset: () => void;
}) {
  const { permission, access, available, system_authorization } = grant;
  const label = managed_permission_labels[permission];
  const unavailable =
    !available && access !== 'unverified_package' ? permission_unavailable : unavailable_reason;
  const can_reset = access !== 'ask';
  const system_label =
    !unavailable && (access === 'remembered' || access === 'ask')
      ? systemPermissionLabel(system_authorization)
      : undefined;
  return (
    <div className="applet-permission-row">
      <dt>{label}</dt>
      <dd>
        <div className="applet-permission-state">
          <span>{unavailable ?? access_labels[access]}</span>
          {unavailable && can_reset && (
            <small>{access === 'remembered' ? 'Approval saved' : access_labels[access]}</small>
          )}
          {system_label && <small>{system_label}</small>}
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

function AppletPermissionGroup({
  applet,
  busy,
  open,
  reset,
}: {
  applet: AppletPermission;
  busy: boolean;
  open?: boolean;
  reset: (permission: ManagedPermission) => void;
}) {
  const rows = applet.manifest ? appletPermissionRows(applet.manifest) : [];
  const managed = rows.filter((row) => row.kind === 'managed');
  const fixed = rows.filter((row) => row.kind === 'fixed');
  const pickers = rows.filter((row) => row.kind === 'picker');
  const configurable = applet.grants.some(
    (grant) => grant.available && (grant.access === 'ask' || grant.access === 'remembered'),
  );
  return (
    <details className="applet-permission-group" open={open}>
      <summary>
        <span className="applet-permission-name">{applet.name}</span>
        {applet.restart_required && (
          <span className="applet-permission-restart">Restart required</span>
        )}
      </summary>
      {(applet.grants.length > 0 || pickers.length > 0) && (
        <div className="applet-permission-section">
          <h3>Device access</h3>
          {configurable && <p className="applet-permission-note">{device_access_note}</p>}
          <dl>
            {applet.grants.map((grant) => (
              <PermissionRow
                key={grant.permission}
                applet_name={applet.name}
                grant={grant}
                busy={busy}
                unavailable_reason={
                  managed.find((row) => row.key === grant.permission)?.unavailable_reason
                }
                reset={() => reset(grant.permission)}
              />
            ))}
            {pickers.map((row) => (
              <div className="applet-permission-row" key={row.key}>
                <dt>{row.label}</dt>
                <dd className="applet-permission-state">{row.status}</dd>
              </div>
            ))}
          </dl>
        </div>
      )}
      {fixed.length > 0 && (
        <div className="applet-permission-section">
          <h3>Package access</h3>
          {applet.restart_required && (
            <p className="applet-permission-note">
              This list shows the installed package’s access. The applet keeps using its previous
              package until you restart it.
            </p>
          )}
          <dl>
            {fixed.map((row) => (
              <div className="applet-permission-row" key={row.key}>
                <dt>{row.label}</dt>
                <dd className="applet-permission-state">{row.status}</dd>
              </div>
            ))}
          </dl>
        </div>
      )}
    </details>
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
    let open = true;
    const unsubscribe = onPermissionsChanged(() => {
      if (open) void load();
    }).catch(() => undefined);
    void load();
    return () => {
      open = false;
      scope.current.cancel();
      void unsubscribe.then((unlisten) => unlisten?.());
    };
  }, []);

  return (
    <section className="panel applet-permissions" aria-labelledby="settings-permissions">
      <h2 id="settings-permissions" className="settings-section-title" tabIndex={-1}>
        Applet permissions
      </h2>
      <p>Review applet permissions and reset saved device approvals.</p>
      {applets?.length ? (
        <div className="applet-permission-groups" aria-busy={busy}>
          {applets.map((applet) => (
            <AppletPermissionGroup
              key={applet.applet_id}
              applet={applet}
              busy={busy}
              open={applets.length === 1 || undefined}
              reset={(permission) => void load({ applet_id: applet.applet_id, permission })}
            />
          ))}
        </div>
      ) : (
        (applets || busy) && (
          <p className="status" role="status">
            {applets ? 'No applet permissions to show.' : 'Loading permissions…'}
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
