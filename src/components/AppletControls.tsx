import { useCallback, useEffect, useId, useRef, useState } from 'react';
import {
  appletPermissionRows,
  managed_permission_labels,
  permission_mode_labels,
} from '../lib/permissions';
import { getPermissionMode, message, setPermissionMode } from '../lib/tauri';
import type { AppletSession, ManagedPermission, PermissionMode } from '../lib/types';
import { Icon } from './Icon';
import { Sheet } from './Sheet';
import { AppletChannel } from './AppletChannel';
import { OperationScope } from './providers/state';
import './AppletControls.css';

function PermissionAccess({
  session,
  permission,
  read_only,
}: {
  session: AppletSession;
  permission: ManagedPermission;
  read_only: boolean;
}) {
  const { session_id, view_id } = session;
  const label_id = useId();
  const [mode, setMode] = useState<PermissionMode | 'unavailable'>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());

  const update = useCallback(
    async (next?: PermissionMode) => {
      if (read_only) return;
      const operation = scope.current.start();
      if (!operation) return;
      setBusy(true);
      setError('');
      try {
        const saved =
          next === undefined
            ? await getPermissionMode(session_id, view_id, permission)
            : await setPermissionMode(session_id, view_id, permission, next);
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
    [read_only, session_id, view_id, permission],
  );

  useEffect(() => {
    const operations = scope.current;
    void update();
    return () => operations.cancel();
  }, [update]);

  return (
    <div>
      <dt id={label_id}>{managed_permission_labels[permission]}</dt>
      <dd>
        {mode === 'unavailable' ? (
          'Unavailable on this platform'
        ) : read_only ? (
          mode ? (
            permission_mode_labels[mode]
          ) : (
            'Not checked'
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
            <option value="ask">{permission_mode_labels.ask}</option>
            <option value="remembered">{permission_mode_labels.remembered}</option>
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
  const [panel, setPanel] = useState<'menu' | 'about' | null>(null);
  const read_only = disabled || Boolean(stopped);
  const device_access_id = useId();
  const package_access_id = useId();
  const rows = appletPermissionRows({ ...manifest, has_lib: session.has_lib });
  const device_rows = rows.filter((row) => row.kind === 'managed' || row.key === 'photos');
  const package_rows = rows
    .filter((row) => row.kind === 'fixed')
    .filter((row) => row.key !== 'photos');
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
                    <p className="applet-about-device-note">
                      Apell’s approvals still depend on your system permissions.
                    </p>
                  )}
                  <dl className="applet-access">
                    {device_rows.map((row) =>
                      row.kind === 'managed' && !row.unavailable_reason ? (
                        <PermissionAccess
                          key={`${session.view_id}-${row.key}`}
                          session={session}
                          permission={row.key}
                          read_only={read_only}
                        />
                      ) : (
                        <div key={row.key}>
                          <dt>{row.label}</dt>
                          <dd>{row.kind === 'fixed' ? row.status : row.unavailable_reason}</dd>
                        </div>
                      ),
                    )}
                  </dl>
                </section>
              )}
              <AppletChannel
                session={session}
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
