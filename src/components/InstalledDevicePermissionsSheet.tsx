import { useCallback, useLayoutEffect, useRef } from 'react';
import { storeSourceLabel } from '../lib/storeState';
import { getAppletInfo } from '../lib/tauri';
import type { InstalledApplet } from '../lib/types';
import { AppletInfoIdentity } from './AppletInfoSheet';
import { AppletPermissionDetails, useAppletPermissionState } from './AppletPermissions';
import { Sheet } from './Sheet';

export function InstalledDevicePermissionsSheet({
  applet,
  onClose,
}: {
  applet: InstalledApplet;
  onClose: () => void;
}) {
  const { applet_id, removal_revision } = applet;
  const installation_changed = useRef(false);
  const changed_notice = useRef<HTMLParagraphElement>(null);
  const read = useCallback(async () => {
    if (installation_changed.current) return null;
    const info = await getAppletInfo(applet_id);
    if (
      info.applet.applet_id !== applet_id ||
      info.permissions.applet_id !== applet_id ||
      info.applet.removal_revision !== removal_revision
    ) {
      installation_changed.current = true;
      return null;
    }
    return info.permissions;
  }, [applet_id, removal_revision]);
  const { value, busy, error, saving, edit_error, load } = useAppletPermissionState(
    read,
    applet_id,
  );
  useLayoutEffect(() => {
    if (value === null && !busy) changed_notice.current?.focus({ preventScroll: true });
  }, [value, busy]);

  return (
    <Sheet open title="Device permissions" dismissible={!saving} onClose={onClose}>
      <div className="applet-info">
        <AppletInfoIdentity applet={applet} />
        <dl className="applet-access applet-info-source installed-permissions-source">
          <div>
            <dt>Installed from</dt>
            <dd>
              <bdi>{applet.source ? storeSourceLabel(applet.source) : 'Local package'}</bdi>
            </dd>
          </div>
        </dl>
        {value === null ? (
          <p ref={changed_notice} tabIndex={-1} className="status" role="status">
            The installed applet changed. Close this sheet and review the current installation.
          </p>
        ) : error ? (
          <div>
            <p className="status error" role="alert">
              Couldn’t refresh device permissions. {error}
            </p>
            <button type="button" disabled={busy} onClick={() => void load()}>
              Try again
            </button>
          </div>
        ) : value ? (
          <>
            {edit_error &&
              !value.grants.some((grant) => grant.permission === edit_error.permission) && (
                <p className="status error" role="alert">
                  {edit_error.text}
                </p>
              )}
            <AppletPermissionDetails
              applet={value}
              device_only
              busy={busy}
              saving={saving?.permission}
              edit_error={edit_error}
              edit={(change) => void load(change)}
            />
          </>
        ) : (
          <p role="status">Checking device permissions…</p>
        )}
      </div>
    </Sheet>
  );
}
