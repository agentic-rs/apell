import { useCallback, useEffect, useId, useRef, useState } from 'react';
import {
  appletPermissionRows,
  device_access_note,
  isPermissionMode,
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
  setInstalledPermissionMode,
} from '../lib/tauri';
import type {
  AppletPermission,
  ManagedPermission,
  PermissionGrant,
  PermissionMode,
} from '../lib/types';
import { SystemPermissionAccess } from './SystemPermissionAccess';
import { OperationScope } from './providers/state';
import './AppletPermissions.css';

const access_labels: Record<PermissionGrant['access'], string> = {
  ...permission_mode_labels,
  previous_package: 'Setting saved for an earlier package',
  undeclared: 'Unused by installed package',
  unverified_package: 'Applet unavailable',
};

interface PermissionTarget {
  applet_id: string;
  permission: ManagedPermission;
}

type PermissionChange = PermissionTarget &
  ({ mode: PermissionMode; installed_revision: string } | { mode?: undefined });

type PermissionChangeError = PermissionTarget & { text: string };

function PermissionRow({
  applet_name,
  grant,
  busy,
  saving,
  error,
  unavailable_reason,
  change,
  reset,
}: {
  applet_name: string;
  grant: PermissionGrant;
  busy: boolean;
  saving: boolean;
  error?: string;
  unavailable_reason?: string;
  change?: (mode: PermissionMode) => void;
  reset: () => void;
}) {
  const id = useId();
  const { permission, access, available, mode, system_authorization } = grant;
  const label = managed_permission_labels[permission];
  const unavailable =
    !available && access !== 'unverified_package' ? permission_unavailable : unavailable_reason;
  const can_reset = access !== 'ask' && (!change || access === 'previous_package');
  const system_label = !unavailable && mode && systemPermissionLabel(system_authorization);
  const description = [
    system_label && `${id}-system`,
    access === 'previous_package' && change && `${id}-previous`,
    error && `${id}-error`,
  ]
    .filter(Boolean)
    .join(' ');
  return (
    <div className="applet-permission-row">
      <dt>{label}</dt>
      <dd>
        <div className="applet-permission-state">
          {change && mode ? (
            <select
              className="applet-permission-mode"
              aria-label={`${label} access for ${applet_name}`}
              aria-describedby={description || undefined}
              value={mode}
              disabled={busy}
              onChange={(event) => {
                const next = event.currentTarget.value;
                if (isPermissionMode(next)) change(next);
              }}
            >
              {Object.entries(permission_mode_labels).map(([key, name]) => (
                <option key={key} value={key}>
                  {name}
                </option>
              ))}
            </select>
          ) : (
            <span>{unavailable ?? access_labels[access]}</span>
          )}
          {((change && access === 'previous_package') || (unavailable && can_reset)) && (
            <small id={`${id}-previous`}>{access_labels[access]}</small>
          )}
          {system_label && mode && (
            <SystemPermissionAccess
              id={`${id}-system`}
              applet_name={applet_name}
              permission={permission}
              authorization={system_authorization}
              mode={mode}
              can_open_system_settings={!!change && grant.can_open_system_settings}
              disabled={busy}
            />
          )}
          {saving && <small role="status">Updating…</small>}
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
        {error && (
          <p className="applet-permission-error error" id={`${id}-error`} role="alert">
            {error}
          </p>
        )}
      </dd>
    </div>
  );
}

interface AppletPermissionDetailsProps {
  applet: AppletPermission;
  busy: boolean;
  saving?: ManagedPermission;
  edit_error?: PermissionChangeError;
  edit: (change: PermissionChange) => void;
}

function AppletPermissionGroup({
  open,
  ...props
}: AppletPermissionDetailsProps & { open?: boolean }) {
  return (
    <details className="applet-permission-group" open={open}>
      <summary>
        <span className="applet-permission-name">{props.applet.name}</span>
        {props.applet.restart_required && (
          <span className="applet-permission-restart">Restart required</span>
        )}
      </summary>
      <AppletPermissionDetails {...props} />
    </details>
  );
}

export function AppletPermissionDetails({
  applet,
  busy,
  saving,
  edit_error,
  edit,
}: AppletPermissionDetailsProps) {
  const rows = applet.manifest ? appletPermissionRows(applet.manifest) : [];
  const managed = rows.filter((row) => row.kind === 'managed');
  const fixed = rows.filter((row) => row.kind === 'fixed');
  const pickers = rows.filter((row) => row.kind === 'picker');
  const { applet_id, installed_revision } = applet;
  const configurable = applet.grants.some((grant) => grant.available && grant.mode);
  return (
    <div className="applet-permission-details">
      {(applet.grants.length > 0 || pickers.length > 0) && (
        <div className="applet-permission-section">
          <h3>Device access</h3>
          {configurable && <p className="applet-permission-note">{device_access_note}</p>}
          <dl>
            {applet.grants.map((grant) => {
              const { permission } = grant;
              const declaration = managed.find((row) => row.key === permission);
              const can_change =
                installed_revision &&
                declaration &&
                !declaration.unavailable_reason &&
                grant.available &&
                grant.mode;
              return (
                <PermissionRow
                  key={permission}
                  applet_name={applet.name}
                  grant={grant}
                  busy={busy}
                  saving={saving === permission}
                  error={edit_error?.permission === permission ? edit_error.text : undefined}
                  unavailable_reason={declaration?.unavailable_reason}
                  change={
                    can_change
                      ? (mode) => edit({ applet_id, installed_revision, permission, mode })
                      : undefined
                  }
                  reset={() => edit({ applet_id, permission })}
                />
              );
            })}
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
    </div>
  );
}

export function useAppletPermissionState<T>(read: () => Promise<T>) {
  const [value, setValue] = useState<T>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const [saving, setSaving] = useState<PermissionTarget>();
  const [edit_error, setEditError] = useState<PermissionChangeError>();
  const scope = useRef(new OperationScope());
  const refresh_pending = useRef(false);

  const load = useCallback(
    async (edit?: PermissionChange) => {
      const operation = scope.current.start();
      if (!operation) {
        if (!edit) refresh_pending.current = true;
        return;
      }
      setBusy(true);
      let latest: T | undefined;
      try {
        if (edit) {
          setSaving(edit);
          setEditError(undefined);
          try {
            if (edit.mode) {
              await setInstalledPermissionMode(
                edit.applet_id,
                edit.installed_revision,
                edit.permission,
                edit.mode,
              );
            } else {
              await forgetPermission(edit.applet_id, edit.permission);
            }
          } catch (cause) {
            if (operation.isCurrent()) {
              setEditError({
                applet_id: edit.applet_id,
                permission: edit.permission,
                text: message(cause),
              });
            }
          }
        }
        while (operation.isCurrent()) {
          refresh_pending.current = false;
          try {
            latest = await read();
            if (!operation.isCurrent()) return;
            setValue(latest);
            setError('');
          } catch (cause) {
            if (!operation.isCurrent()) return;
            latest = undefined;
            setError(message(cause));
          }
          if (!refresh_pending.current) break;
        }
        return latest;
      } finally {
        if (operation.isCurrent()) {
          operation.finish();
          setSaving(undefined);
          setBusy(false);
        }
      }
    },
    [read],
  );

  useEffect(() => {
    let open = true;
    setBusy(true);
    setSaving(undefined);
    const unsubscribe = onPermissionsChanged(() => {
      if (open) void load();
    }).catch(() => undefined);
    void unsubscribe.then(() => {
      if (open) void load();
    });
    return () => {
      open = false;
      scope.current.cancel();
      refresh_pending.current = false;
      void unsubscribe.then((unlisten) => unlisten?.());
    };
  }, [load]);

  return { value, busy, error, saving, edit_error, load };
}

export function AppletPermissions() {
  const {
    value: applets,
    busy,
    error,
    saving,
    edit_error,
    load,
  } = useAppletPermissionState(listAppletPermissions);
  return (
    <section className="panel applet-permissions" aria-labelledby="settings-permissions">
      <h2 id="settings-permissions" className="settings-section-title" tabIndex={-1}>
        Applet permissions
      </h2>
      <p>Manage device permissions and review each applet’s declared access.</p>
      {error && (
        <div role="alert">
          <p className="status error">Couldn’t refresh permissions. {error}</p>
          <button type="button" disabled={busy} onClick={() => void load()}>
            Try again
          </button>
        </div>
      )}
      {edit_error &&
        !applets?.some(
          (applet) =>
            applet.applet_id === edit_error.applet_id &&
            applet.grants.some((grant) => grant.permission === edit_error.permission),
        ) && (
          <p className="status error" role="alert">
            {edit_error.text}
          </p>
        )}
      {applets?.length ? (
        <div className="applet-permission-groups" aria-busy={busy}>
          {applets.map((applet) => (
            <AppletPermissionGroup
              key={applet.applet_id}
              applet={applet}
              busy={busy || !!error}
              open={applets.length === 1 || undefined}
              saving={saving?.applet_id === applet.applet_id ? saving.permission : undefined}
              edit_error={edit_error?.applet_id === applet.applet_id ? edit_error : undefined}
              edit={(change) => void load(change)}
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
    </section>
  );
}
