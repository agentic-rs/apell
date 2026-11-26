import { useEffect, useRef, useState } from 'react';
import { managed_permission_labels, systemPermissionLabel } from '../lib/permissions';
import { message, openPermissionSystemSettings } from '../lib/tauri';
import type { ManagedPermission, PermissionMode, SystemAuthorization } from '../lib/types';
import { OperationScope } from './providers/state';
import './SystemPermissionAccess.css';

export function SystemPermissionAccess({
  id,
  applet_name,
  permission,
  authorization,
  mode,
  can_open_system_settings,
  disabled,
}: {
  id: string;
  applet_name: string;
  permission: ManagedPermission;
  authorization: SystemAuthorization;
  mode: PermissionMode;
  can_open_system_settings?: boolean;
  disabled: boolean;
}) {
  const label = systemPermissionLabel(authorization);
  if (!label) return null;
  const can_open =
    can_open_system_settings === true &&
    (mode === 'ask' || mode === 'remembered') &&
    (authorization === 'blocked' || authorization === 'not_granted');
  return (
    <div className="system-permission-access">
      <small id={id}>{label}</small>
      {can_open && (
        <OpenSystemSettings
          applet_name={applet_name}
          permission={permission}
          status_id={id}
          disabled={disabled}
        />
      )}
    </div>
  );
}

function OpenSystemSettings({
  applet_name,
  permission,
  status_id,
  disabled,
}: {
  applet_name: string;
  permission: ManagedPermission;
  status_id: string;
  disabled: boolean;
}) {
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());
  useEffect(() => {
    const operations = scope.current;
    setOpening(false);
    return () => operations.cancel();
  }, []);

  const open = async () => {
    const operation = scope.current.start();
    if (!operation) return;
    setOpening(true);
    setError('');
    try {
      await openPermissionSystemSettings(permission);
    } catch (cause) {
      if (operation.isCurrent()) setError(message(cause));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setOpening(false);
      }
    }
  };

  return (
    <>
      <button
        type="button"
        className="system-permission-button"
        aria-label={`Open system settings, ${managed_permission_labels[permission]} access for ${applet_name}`}
        aria-describedby={error ? `${status_id} ${status_id}-error` : status_id}
        disabled={disabled || opening}
        onClick={() => void open()}
      >
        {opening ? 'Opening settings…' : 'Open system settings'}
      </button>
      {error && (
        <p className="system-permission-error" id={`${status_id}-error`} role="alert">
          {error}
        </p>
      )}
    </>
  );
}
