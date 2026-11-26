import { useCallback, useLayoutEffect, useRef } from 'react';
import { getUnavailableAppletPermissions } from '../lib/tauri';
import { AppletPermissionDetails, useAppletPermissionState } from './AppletPermissions';
import { Sheet } from './Sheet';

export function UnavailableAppletPermissions({
  applet_id,
  onClose,
}: {
  applet_id: string;
  onClose: (installation_changed: boolean) => void;
}) {
  const read = useCallback(() => getUnavailableAppletPermissions(applet_id), [applet_id]);
  const { value, busy, error, saving, edit_error, load } = useAppletPermissionState(
    read,
    applet_id,
  );
  const identity = useRef<HTMLParagraphElement>(null);
  const reset_focus = useRef(false);
  const installation_changed = value === null;

  useLayoutEffect(() => {
    if (busy || (!reset_focus.current && !installation_changed)) return;
    reset_focus.current = false;
    identity.current?.focus({ preventScroll: true });
  }, [busy, installation_changed, value]);

  return (
    <Sheet
      open
      title="Device permissions"
      dismissible={!saving}
      onClose={() => onClose(installation_changed)}
    >
      <div className="unavailable-permissions">
        <p ref={identity} tabIndex={-1}>
          <strong>
            <bdi>{applet_id}</bdi>
          </strong>
        </p>
        {error && (
          <div>
            <p className="status error" role="alert">
              Couldn’t refresh saved permissions. {error}
            </p>
            <button type="button" disabled={busy} onClick={() => void load()}>
              Try again
            </button>
          </div>
        )}
        {edit_error &&
          !value?.grants.some((grant) => grant.permission === edit_error.permission) && (
            <p className="status error" role="alert">
              {edit_error.text}
            </p>
          )}
        {installation_changed ? (
          <p role="status">
            This applet’s installation changed. Close this sheet to review Library.
          </p>
        ) : value?.grants.length ? (
          <>
            <p>
              Reset a saved choice to make Apell ask again when the restored applet requests access.
            </p>
            <AppletPermissionDetails
              applet={value}
              busy={busy || !!error}
              saving={saving?.permission}
              edit_error={edit_error}
              edit={(change) => {
                if (change.mode !== undefined) return;
                reset_focus.current = true;
                void load(change);
              }}
            />
          </>
        ) : value ? (
          <p role="status">No saved device permissions.</p>
        ) : busy ? (
          <p role="status">Checking saved permissions…</p>
        ) : null}
      </div>
    </Sheet>
  );
}
