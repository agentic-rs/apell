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
  getAppletInstallation,
  getPermissionMode,
  message,
  onAppletInstallationChanged,
  onMicrophoneRecordingChanged,
  onPermissionsChanged,
  setPermissionMode,
} from '../lib/tauri';
import type {
  AppletSession,
  ManagedPermission,
  PermissionMode,
  ViewPermissionStatus,
} from '../lib/types';
import { Icon } from './Icon';
import { Sheet } from './Sheet';
import { AppletChannel } from './AppletChannel';
import { SystemPermissionAccess } from './SystemPermissionAccess';
import { OperationScope } from './providers/state';
import './AppletControls.css';

const edit_blocker_labels = {
  source_changed: 'Restart this applet before changing permissions for its new source.',
  permission_removed:
    'The installed version no longer requests this permission. Restart to open the installed version.',
  installation_unavailable: 'The installed package is unavailable. Open Library to check it.',
  microphone_recording_open: 'Finish the current recording to change microphone access.',
} satisfies Record<NonNullable<ViewPermissionStatus['edit_blocker']>, string>;

function PermissionAccess({
  session,
  permission,
  read_only,
  unavailable_reason,
}: {
  session: AppletSession;
  permission: ManagedPermission;
  read_only: boolean;
  unavailable_reason?: string;
}) {
  const { session_id, view_id } = session;
  const { applet_id } = session.manifest;
  const label_id = useId();
  const [status, setStatus] = useState<ViewPermissionStatus>();
  const [busy, setBusy] = useState(true);
  const [read_error, setReadError] = useState('');
  const [edit_error, setEditError] = useState('');
  const [refresh_requested, setRefreshRequested] = useState(false);
  const scope = useRef(new OperationScope());

  const update = useCallback(
    async (next?: PermissionMode) => {
      if (read_only || unavailable_reason) return;
      const operation = scope.current.start();
      if (!operation) return;
      setBusy(true);
      setReadError('');
      setEditError('');
      try {
        if (next !== undefined) {
          try {
            const saved = await setPermissionMode(session_id, view_id, permission, next);
            if (operation.isCurrent()) setStatus(saved);
            return;
          } catch (error) {
            if (!operation.isCurrent()) return;
            setEditError(message(error));
          }
        }
        const saved = await getPermissionMode(session_id, view_id, permission);
        if (operation.isCurrent()) setStatus(saved);
      } catch (error) {
        if (operation.isCurrent()) setReadError(message(error));
      } finally {
        if (operation.isCurrent()) {
          operation.finish();
          setBusy(false);
        }
      }
    },
    [read_only, session_id, view_id, permission, unavailable_reason],
  );

  useEffect(() => {
    if (read_only || unavailable_reason) return;
    const operations = scope.current;
    let open = true;
    const refresh = () => {
      if (open) setRefreshRequested(true);
    };
    const subscriptions = [
      onPermissionsChanged(refresh),
      onAppletInstallationChanged((changed_id) => {
        if (changed_id === applet_id) refresh();
      }),
    ];
    if (permission === 'microphone') subscriptions.push(onMicrophoneRecordingChanged(refresh));
    const unsubscribe = subscriptions.map((subscription) => subscription.catch(() => undefined));
    void Promise.all(unsubscribe).then(() => {
      if (open) void update();
    });
    return () => {
      open = false;
      operations.cancel();
      for (const stop of unsubscribe) void stop.then((unlisten) => unlisten?.());
    };
  }, [applet_id, permission, read_only, unavailable_reason, update]);

  useEffect(() => {
    if (!refresh_requested || busy || read_only || unavailable_reason) return;
    setRefreshRequested(false);
    void update();
  }, [refresh_requested, busy, read_only, unavailable_reason, update]);

  const mode = status?.mode;
  const blocker = !read_error && status?.edit_blocker && edit_blocker_labels[status.edit_blocker];
  const locked = read_only || !!read_error || !!blocker;
  const refreshing = busy || refresh_requested;
  const problem = read_error || (!blocker && edit_error);
  const system_label =
    status && !read_error && mode !== 'unavailable' && !unavailable_reason
      ? systemPermissionLabel(status.system_authorization)
      : undefined;

  return (
    <div>
      <dt id={label_id}>{managed_permission_labels[permission]}</dt>
      <dd>
        {unavailable_reason ? (
          unavailable_reason
        ) : mode === 'unavailable' ? (
          permission_unavailable
        ) : locked ? (
          mode ? (
            permission_mode_labels[mode]
          ) : (
            'Not checked'
          )
        ) : mode ? (
          <select
            aria-labelledby={label_id}
            aria-describedby={system_label ? `${label_id}-system` : undefined}
            value={mode}
            disabled={refreshing}
            onChange={(event) => {
              const value = event.currentTarget.value;
              if (isPermissionMode(value)) void update(value);
            }}
          >
            {Object.entries(permission_mode_labels).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        ) : refreshing ? (
          <span role="status">Loading…</span>
        ) : null}
        {system_label && status && mode && mode !== 'unavailable' && (
          <SystemPermissionAccess
            id={`${label_id}-system`}
            applet_name={session.manifest.name}
            permission={permission}
            authorization={status.system_authorization}
            mode={mode}
            can_open_system_settings={!read_only && status.can_open_system_settings}
            disabled={refreshing}
          />
        )}
      </dd>
      {blocker && (
        <dd className="applet-access-note">
          <span role="status">{blocker}</span>
        </dd>
      )}
      {!read_only && problem && (
        <dd className="applet-access-error">
          <span className="status error" role="alert">
            {problem}
          </span>
          {read_error && (
            <button type="button" disabled={refreshing} onClick={() => void update()}>
              Try again
            </button>
          )}
        </dd>
      )}
    </div>
  );
}

interface AppletControlsProps {
  session: AppletSession;
  disabled: boolean;
  stopped?: boolean;
  installing_channel: boolean;
  onInstallingChannel: (installing: boolean) => void;
  onClose: () => void;
  onReload: () => void;
  onRestart: () => void;
}

export function AppletControls({
  session,
  disabled,
  stopped,
  installing_channel,
  onInstallingChannel,
  onClose,
  onReload,
  onRestart,
}: AppletControlsProps) {
  const { manifest } = session;
  const { session_id, view_id } = session;
  const loadInstallation = useCallback(
    () => getAppletInstallation(session_id, view_id),
    [session_id, view_id],
  );
  const [panel, setPanel] = useState<'menu' | 'about' | null>(null);
  const read_only = disabled || Boolean(stopped);
  const device_access_id = useId();
  const package_access_id = useId();
  const rows = appletPermissionRows({ ...manifest, has_lib: session.has_lib });
  const device_rows = rows.filter((row) => row.kind !== 'fixed');
  const package_rows = rows.filter((row) => row.kind === 'fixed');
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
          dismissible={!installing_channel}
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
              {device_rows.length > 0 && (
                <section aria-labelledby={device_access_id}>
                  <h4 id={device_access_id}>Device access</h4>
                  {device_rows.some((row) => row.kind === 'managed' && !row.unavailable_reason) && (
                    <p className="applet-about-device-note">{device_access_note}</p>
                  )}
                  <dl className="applet-access">
                    {device_rows.map((row) =>
                      row.kind === 'managed' ? (
                        <PermissionAccess
                          key={`${session.view_id}-${row.key}`}
                          session={session}
                          permission={row.key}
                          read_only={read_only}
                          unavailable_reason={row.unavailable_reason}
                        />
                      ) : (
                        <div key={row.key}>
                          <dt>{row.label}</dt>
                          <dd>{row.status}</dd>
                        </div>
                      ),
                    )}
                  </dl>
                </section>
              )}
              <AppletChannel
                key={view_id}
                applet_id={manifest.applet_id}
                loadInstallation={loadInstallation}
                disabled={read_only}
                onInstalling={onInstallingChannel}
                onRestart={() => {
                  setPanel(null);
                  onRestart();
                }}
              />
              <section aria-labelledby={package_access_id}>
                <h4 id={package_access_id}>Package access</h4>
                <dl className="applet-access">
                  {package_rows.map((row) => (
                    <div key={row.key}>
                      <dt>{row.label}</dt>
                      <dd>{row.status}</dd>
                    </div>
                  ))}
                </dl>
              </section>
            </div>
          )}
        </Sheet>
      )}
    </>
  );
}
