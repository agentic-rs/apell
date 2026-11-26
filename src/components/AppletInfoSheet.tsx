import { useCallback, useMemo, useState } from 'react';
import { getAppletInfo } from '../lib/tauri';
import { storeSourceLabel } from '../lib/storeState';
import type {
  AppletInfo,
  AppletInstallResult,
  AppletInstallation,
  InstalledApplet,
} from '../lib/types';
import { AppletChannel } from './AppletChannel';
import { AppletIcon } from './AppletIcon';
import { AppletPermissionDetails, useAppletPermissionState } from './AppletPermissions';
import { Sheet } from './Sheet';
import './AppletInfoSheet.css';

function installation({ applet, permissions }: AppletInfo): AppletInstallation {
  return {
    store_source: applet.source?.kind === 'store' ? applet.source : null,
    github_source: applet.source?.kind === 'github' ? applet.source : undefined,
    installed_version: applet.version,
    installed_sha256: applet.sha256,
    restart_required: permissions.restart_required,
  };
}

export function AppletInfoIdentity({ applet }: { applet: InstalledApplet }) {
  return (
    <div className="applet-info-identity">
      <span className="applet-icon" aria-hidden="true">
        <AppletIcon applet={applet} />
      </span>
      <div>
        <h3>{applet.name}</h3>
        <p>Version {applet.version}</p>
      </div>
    </div>
  );
}

export function AppletInfoSheet({
  applet,
  onClose,
  onInstalled,
  onRestart,
}: {
  applet: InstalledApplet;
  onClose: () => void;
  onInstalled: (result: AppletInstallResult) => void;
  onRestart: (applet: InstalledApplet) => void;
}) {
  const { applet_id } = applet;
  const read = useCallback(() => getAppletInfo(applet_id), [applet_id]);
  const {
    value: info,
    busy,
    error,
    saving,
    edit_error,
    load,
  } = useAppletPermissionState(read, applet_id);
  const [installing, setInstalling] = useState(false);
  const current = info?.applet ?? applet;
  const initial_installation = useMemo(() => (info ? installation(info) : undefined), [info]);
  const loadInstallation = useCallback(async () => {
    const refreshed = await load();
    if (!refreshed) throw new Error('Could not refresh applet info. Try again.');
    return installation(refreshed);
  }, [load]);

  return (
    <Sheet open title="Applet info" dismissible={!installing && !saving} onClose={onClose}>
      <div className="applet-info">
        <AppletInfoIdentity applet={current} />
        {error && (
          <div role="alert">
            <p className="status error">Couldn’t refresh applet info. {error}</p>
            <button type="button" disabled={busy || installing} onClick={() => void load()}>
              Try again
            </button>
          </div>
        )}
        {!info && busy && <p role="status">Loading applet info…</p>}
        {info && (
          <>
            <div>
              <dl className="applet-access applet-info-source">
                <div>
                  <dt>Installed from</dt>
                  <dd>{current.source ? storeSourceLabel(current.source) : 'Local package'}</dd>
                </div>
              </dl>
              <AppletChannel
                applet_id={applet_id}
                initial_installation={initial_installation}
                loadInstallation={loadInstallation}
                disabled={busy || !!error}
                onInstalling={setInstalling}
                onInstalled={(result) => {
                  onInstalled(result);
                  void load();
                }}
                onRestart={() => onRestart(current)}
              />
            </div>
            {edit_error &&
              !info.permissions.grants.some(
                (grant) => grant.permission === edit_error.permission,
              ) && (
                <p className="status error" role="alert">
                  {edit_error.text}
                </p>
              )}
            <AppletPermissionDetails
              applet={info.permissions}
              busy={busy || installing || !!error}
              saving={saving?.permission}
              edit_error={edit_error}
              edit={(change) => void load(change)}
            />
          </>
        )}
      </div>
    </Sheet>
  );
}
