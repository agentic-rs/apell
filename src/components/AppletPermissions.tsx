import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import {
  appletPermissionRows,
  device_access_note,
  groupDevicePermissions,
  groupSystemPermission,
  isPermissionMode,
  managedPermissionDeclaration,
  managed_permission_labels,
  matchingPermissionApplets,
  permission_mode_labels,
  permission_unavailable,
  systemPermissionLabel,
} from '../lib/permissions';
import {
  forgetPermission,
  listAppletPermissions,
  message,
  onAppletInstallationChanged,
  onPermissionsChanged,
  setInstalledPermissionMode,
} from '../lib/tauri';
import type {
  AppletPermission,
  ManagedPermission,
  PermissionGrant,
  PermissionMode,
} from '../lib/types';
import { OpenSystemSettings, SystemPermissionAccess } from './SystemPermissionAccess';
import { Icon } from './Icon';
import { OperationScope } from './providers/state';
import './AppletPermissions.css';

const access_labels: Record<PermissionGrant['access'], string> = {
  ...permission_mode_labels,
  previous_package: 'Setting saved for an earlier package',
  undeclared: 'Unused by installed package',
  unverified_package: 'Saved setting',
};

interface PermissionTarget {
  applet_id: string;
  permission: ManagedPermission;
}

type PermissionChange = PermissionTarget & { grant_revision: string } & (
    { mode: PermissionMode; installed_revision: string } | { mode?: undefined }
  );

type PermissionChangeError = PermissionTarget & { text: string };

function PermissionRow({
  applet_name,
  title,
  grant,
  busy,
  saving,
  error,
  unavailable_reason,
  system_status_id,
  change,
  reset,
}: {
  applet_name: string;
  title?: ReactNode;
  grant: PermissionGrant;
  busy: boolean;
  saving: boolean;
  error?: string;
  unavailable_reason?: string;
  system_status_id?: string;
  change?: (mode: PermissionMode) => void;
  reset: () => void;
}) {
  const id = useId();
  const { permission, access, available, mode, system_authorization } = grant;
  const label = managed_permission_labels[permission];
  const unavailable =
    !available && access !== 'unverified_package' ? permission_unavailable : unavailable_reason;
  const saved_label = grant.saved_mode && `Saved as ${permission_mode_labels[grant.saved_mode]}`;
  const can_reset = access !== 'ask' && (!change || access === 'previous_package');
  const system_label = !unavailable && mode && systemPermissionLabel(system_authorization);
  const description = [
    system_label && (system_status_id ?? `${id}-system`),
    access === 'previous_package' && change && `${id}-previous`,
    error && `${id}-error`,
  ]
    .filter(Boolean)
    .join(' ');
  return (
    <div className="applet-permission-row">
      <dt>{title ?? label}</dt>
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
            <span>{unavailable ?? saved_label ?? access_labels[access]}</span>
          )}
          {((change && access === 'previous_package') || (unavailable && can_reset)) && (
            <small id={`${id}-previous`}>{access_labels[access]}</small>
          )}
          {system_label && mode && !system_status_id && (
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
  show_id?: boolean;
  busy: boolean;
  saving?: ManagedPermission;
  edit_error?: PermissionChangeError;
  edit: (change: PermissionChange) => void;
}

function appletPermissionStatus(applet: AppletPermission) {
  if (applet.manifest === null) return 'Applet unavailable';
  if (applet.restart_required) return 'Restart required';
}

function AppletDeviceGrant({
  applet,
  show_id,
  grant,
  busy,
  saving,
  edit_error,
  edit,
  show_applet = false,
  system_status_id,
}: AppletPermissionDetailsProps & {
  grant: PermissionGrant;
  show_applet?: boolean;
  system_status_id?: string;
}) {
  const { applet_id, installed_revision } = applet;
  const { permission, grant_revision } = grant;
  const target = { applet_id, permission, grant_revision };
  const declaration = managedPermissionDeclaration(applet.manifest, permission);
  const applet_status = appletPermissionStatus(applet);
  const can_change =
    installed_revision &&
    declaration &&
    !declaration.unavailable_reason &&
    grant.available &&
    grant.mode;
  return (
    <PermissionRow
      applet_name={show_id ? `${applet.name} (${applet_id})` : applet.name}
      title={
        show_applet ? (
          <>
            <bdi>{applet.name}</bdi>
            {show_id && <small className="applet-permission-identity">{applet_id}</small>}
            {applet_status && (
              <small className="applet-permission-package-status">{applet_status}</small>
            )}
          </>
        ) : undefined
      }
      grant={grant}
      busy={busy}
      saving={saving === permission}
      error={edit_error?.permission === permission ? edit_error.text : undefined}
      unavailable_reason={declaration?.unavailable_reason}
      system_status_id={system_status_id}
      change={can_change ? (mode) => edit({ ...target, installed_revision, mode }) : undefined}
      reset={() => edit(target)}
    />
  );
}

function AppletPermissionGroup({
  open,
  ...props
}: AppletPermissionDetailsProps & { open?: boolean }) {
  const applet_status = appletPermissionStatus(props.applet);
  return (
    <details className="applet-permission-group" open={open}>
      <summary>
        <span className="applet-permission-name">
          <bdi>{props.applet.name}</bdi>
          {props.show_id && (
            <small className="applet-permission-identity">{props.applet.applet_id}</small>
          )}
        </span>
        {applet_status && <span className="applet-permission-package-status">{applet_status}</span>}
      </summary>
      <AppletPermissionDetails {...props} />
    </details>
  );
}

export function AppletPermissionDetails({
  applet,
  show_id,
  device_only = false,
  busy,
  saving,
  edit_error,
  edit,
}: AppletPermissionDetailsProps & { device_only?: boolean }) {
  const rows = applet.manifest ? appletPermissionRows(applet.manifest) : [];
  const fixed = rows.filter((row) => row.kind === 'fixed');
  const pickers = rows.filter((row) => row.kind === 'picker');
  const configurable = applet.grants.some((grant) => grant.available && grant.mode);
  const has_device_access = applet.grants.length > 0 || pickers.length > 0;
  if (device_only && !has_device_access) {
    return <p className="status">This applet has no device permissions.</p>;
  }
  return (
    <div className="applet-permission-details">
      {has_device_access && (
        <div className="applet-permission-section">
          <h3>Device access</h3>
          {configurable && <p className="applet-permission-note">{device_access_note}</p>}
          <dl>
            {applet.grants.map((grant) => (
              <AppletDeviceGrant
                key={grant.permission}
                applet={applet}
                show_id={show_id}
                grant={grant}
                busy={busy}
                saving={saving}
                edit_error={edit_error}
                edit={edit}
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
      {!device_only && fixed.length > 0 && (
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

export function useAppletPermissionState<T>(read: () => Promise<T>, applet_id?: string) {
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
                edit.grant_revision,
              );
            } else {
              await forgetPermission(edit.applet_id, edit.permission, edit.grant_revision);
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
    const refresh = () => {
      if (open) void load();
    };
    const unsubscribe = [
      onPermissionsChanged(refresh),
      onAppletInstallationChanged((changed_id) => {
        if (!applet_id || changed_id === applet_id) refresh();
      }),
    ].map((subscription) => subscription.catch(() => undefined));
    void Promise.all(unsubscribe).then(refresh);
    return () => {
      open = false;
      scope.current.cancel();
      refresh_pending.current = false;
      for (const stop of unsubscribe) void stop.then((unlisten) => unlisten?.());
    };
  }, [applet_id, load]);

  return { value, busy, error, saving, edit_error, load };
}

export function AppletPermissions() {
  const [view, setView] = useState<'applet' | 'permission'>('applet');
  const [query, setQuery] = useState('');
  const search = useRef<HTMLInputElement>(null);
  const empty_result = useRef<HTMLParagraphElement>(null);
  const focused_row = useRef<{ element: HTMLElement; summary?: HTMLElement | null }>(undefined);
  const search_id = useId();
  const {
    value: applets,
    busy,
    error,
    saving,
    edit_error,
    load,
  } = useAppletPermissionState(listAppletPermissions);
  const matches = matchingPermissionApplets(applets ?? [], query);
  const filtered = query.trim().length > 0;
  const permissions = view === 'permission' ? groupDevicePermissions(matches) : [];
  const groups = filtered ? permissions.filter(({ entries }) => entries.length > 0) : permissions;
  let empty_message = '';
  if (!applets && busy) empty_message = 'Loading permissions…';
  else if (applets && !matches.length) {
    empty_message = filtered ? 'No matching applets.' : 'No applet permissions to show.';
  } else if (filtered && view === 'permission' && !groups.length) {
    empty_message = 'Matching applets have no microphone, camera, or location settings.';
  }
  useLayoutEffect(() => {
    const previous = focused_row.current;
    if (previous?.element.isConnected !== false) return;
    focused_row.current = undefined;
    if (document.activeElement === document.body) {
      const summary = previous.summary?.isConnected ? previous.summary : undefined;
      (summary ?? empty_result.current ?? search.current)?.focus();
    }
  });
  const names = new Map<string, number>();
  for (const { name } of applets ?? []) names.set(name, (names.get(name) ?? 0) + 1);
  const controls = (applet_id: string) => ({
    busy: busy || !!error,
    saving: saving?.applet_id === applet_id ? saving.permission : undefined,
    edit_error: edit_error?.applet_id === applet_id ? edit_error : undefined,
    edit: (change: PermissionChange) => void load(change),
  });
  return (
    <section className="panel applet-permissions" aria-labelledby="settings-permissions">
      <h2 id="settings-permissions" className="settings-section-title" tabIndex={-1}>
        Applet permissions
      </h2>
      {error && (
        <div role="alert">
          <p className="status error">Couldn’t refresh permissions. {error}</p>
          <button type="button" disabled={busy} onClick={() => void load()}>
            Try again
          </button>
        </div>
      )}
      {edit_error &&
        !matches.some(
          (applet) =>
            applet.applet_id === edit_error.applet_id &&
            applet.grants.some((grant) => grant.permission === edit_error.permission),
        ) && (
          <p className="status error" role="alert">
            {edit_error.text}
          </p>
        )}
      {applets && (
        <div className="applet-permission-tools">
          <div className="applet-permission-switch" role="group" aria-label="Group permissions">
            <button
              type="button"
              aria-pressed={view === 'applet'}
              onClick={(event) => {
                event.currentTarget.focus({ preventScroll: true });
                setView('applet');
              }}
            >
              By applet
            </button>
            <button
              type="button"
              aria-pressed={view === 'permission'}
              onClick={(event) => {
                event.currentTarget.focus({ preventScroll: true });
                setView('permission');
              }}
            >
              By permission
            </button>
          </div>
          <div className="applet-permission-search">
            <label htmlFor={search_id}>Find applets</label>
            <div className="applet-permission-search-field">
              <input
                ref={search}
                id={search_id}
                type="search"
                value={query}
                maxLength={200}
                placeholder="Name or applet ID"
                autoCapitalize="none"
                autoCorrect="off"
                onChange={(event) => setQuery(event.currentTarget.value)}
              />
              {query && (
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Clear applet search"
                  title="Clear applet search"
                  onClick={() => {
                    setQuery('');
                    search.current?.focus();
                  }}
                >
                  <Icon name="close" />
                </button>
              )}
            </div>
          </div>
        </div>
      )}
      {empty_message ? (
        <p ref={empty_result} tabIndex={-1} className="status" role="status">
          {empty_message}
        </p>
      ) : (
        applets && (
          <>
            {view === 'permission' && (
              <p className="applet-permission-overview-note">{device_access_note}</p>
            )}
            <div
              className="applet-permission-groups"
              aria-busy={busy}
              onFocus={(event) => {
                focused_row.current = {
                  element: event.target,
                  summary: event.target.closest('details')?.querySelector('summary'),
                };
              }}
            >
              {view === 'applet'
                ? matches.map((applet) => (
                    <AppletPermissionGroup
                      key={applet.applet_id}
                      applet={applet}
                      show_id={(names.get(applet.name) ?? 0) > 1}
                      open={matches.length === 1 || undefined}
                      {...controls(applet.applet_id)}
                    />
                  ))
                : groups.map(({ permission, entries, allowed_count }) => {
                    const name = managed_permission_labels[permission];
                    const system = groupSystemPermission(entries);
                    const system_id = `${search_id}-${permission}-system`;
                    return entries.length ? (
                      <details
                        key={permission}
                        className="applet-permission-group"
                        open={filtered || undefined}
                      >
                        <summary>
                          <span className="applet-permission-name">{name}</span>
                          <span className="applet-permission-group-state">
                            <span className="applet-permission-count">
                              {allowed_count} allowed in Apell
                            </span>
                            {system && (
                              <small id={system_id} className="applet-permission-system-status">
                                {systemPermissionLabel(system.authorization)}
                              </small>
                            )}
                          </span>
                        </summary>
                        {system?.can_open_system_settings && (
                          <div className="applet-permission-system-action system-permission-access">
                            <OpenSystemSettings
                              permission={permission}
                              status_id={system_id}
                              disabled={busy || !!error}
                            />
                          </div>
                        )}
                        <dl className="applet-permission-subjects">
                          {entries.map(({ applet, grant }) => (
                            <AppletDeviceGrant
                              key={applet.applet_id}
                              applet={applet}
                              show_id={(names.get(applet.name) ?? 0) > 1}
                              grant={grant}
                              system_status_id={
                                system?.authorization === grant.system_authorization
                                  ? system_id
                                  : undefined
                              }
                              show_applet
                              {...controls(applet.applet_id)}
                            />
                          ))}
                        </dl>
                      </details>
                    ) : (
                      <div key={permission} className="applet-permission-group">
                        <div className="applet-permission-empty">
                          <span className="applet-permission-name">{name}</span>
                          <span className="applet-permission-count">No applets</span>
                        </div>
                      </div>
                    );
                  })}
            </div>
          </>
        )
      )}
    </section>
  );
}
