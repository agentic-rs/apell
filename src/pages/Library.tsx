import { useEffect, useRef, useState } from 'react';
import {
  installBundle,
  updateApplet,
  launchApplet,
  listApplets,
  message,
  onInstallProgress,
  removeApplet,
  restartApplet,
} from '../lib/tauri';
import type {
  AppletInstallResult,
  AppletListReport,
  AppletSession,
  InstalledApplet,
  UnavailableApplet,
} from '../lib/types';
import { InstallSheet } from '../components/InstallSheet';
import { AppletTile } from '../components/AppletTile';
import { AppletInfoSheet } from '../components/AppletInfoSheet';
import { Icon } from '../components/Icon';
import { Sheet } from '../components/Sheet';

interface AppletAction {
  applet_id: string;
  name: string;
  panel: 'actions' | 'info' | 'remove';
}

interface LibraryProps {
  onLaunch: (session: AppletSession) => void;
  onBusyChange: (busy: boolean) => void;
  installOpen: boolean;
  onInstallOpen: () => void;
  onInstallClose: () => void;
}

export function Library({
  onLaunch,
  onBusyChange,
  installOpen,
  onInstallOpen,
  onInstallClose,
}: LibraryProps) {
  const [applets, setApplets] = useState<InstalledApplet[]>([]);
  const [unavailable, setUnavailable] = useState<UnavailableApplet[]>([]);
  const [loading, setLoading] = useState(true);
  const [load_error, setLoadError] = useState('');
  const [status, setStatus] = useState('');
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [launching_id, setLaunchingId] = useState('');
  const [restart_required, setRestartRequired] = useState<string[]>([]);
  const [restart_offer_id, setRestartOfferId] = useState<string>();
  const [action, setAction] = useState<AppletAction>();
  const pending = useRef(false);
  const list_revision = useRef(0);
  const library = useRef<HTMLElement>(null);
  const restore_library_focus = useRef(false);
  const restart_offer = applets.find(
    (applet) =>
      applet.applet_id === restart_offer_id && restart_required.includes(applet.applet_id),
  );
  const action_applet = applets.find((applet) => applet.applet_id === action?.applet_id);

  const applyReport = (report: AppletListReport) => {
    setApplets(report.applets);
    setUnavailable(report.unavailable);
    setRestartRequired(report.restart_required ?? []);
    setLoadError('');
  };

  useEffect(() => {
    if (busy || !restore_library_focus.current) return;
    restore_library_focus.current = false;
    library.current?.querySelector('button')?.focus({ preventScroll: true });
  }, [busy, action]);

  useEffect(() => {
    let disposed = false;
    const revision = ++list_revision.current;
    const isCurrent = () => !disposed && revision === list_revision.current;
    listApplets()
      .then((report) => {
        if (isCurrent()) applyReport(report);
      })
      .catch((error) => {
        if (isCurrent()) setLoadError(message(error));
      })
      .finally(() => {
        if (isCurrent()) setLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, []);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    onInstallProgress((event) => {
      if (!disposed && pending.current)
        setStatus(
          `Downloading… ${Math.round(event.received_bytes / 1024)} KiB${event.total_bytes ? ` of ${Math.round(event.total_bytes / 1024)} KiB` : ''}`,
        );
    })
      .then((fn) => {
        if (disposed) fn();
        else stop = fn;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  const run = async (action: () => Promise<void>, label: string) => {
    if (pending.current) return;
    pending.current = true;
    onBusyChange(true);
    setBusy(true);
    setFailed(false);
    setStatus(label);
    try {
      await action();
    } catch (error) {
      setStatus(message(error));
      setFailed(true);
    } finally {
      pending.current = false;
      onBusyChange(false);
      setBusy(false);
      setLaunchingId('');
    }
  };
  const refresh = async () => {
    const revision = ++list_revision.current;
    try {
      const report = await listApplets();
      if (revision === list_revision.current) applyReport(report);
    } catch (error) {
      if (revision === list_revision.current) throw error;
    }
  };
  const installed = async ({ applet, restart_required: needs_restart }: AppletInstallResult) => {
    const { applet_id, name, version } = applet;
    setApplets((current) =>
      current.some((item) => item.applet_id === applet_id)
        ? current.map((item) => (item.applet_id === applet_id ? applet : item))
        : [...current, applet],
    );
    setUnavailable((current) => current.filter((item) => item.applet_id !== applet_id));
    setRestartRequired((current) => {
      const other_applets = current.filter((id) => id !== applet_id);
      return needs_restart ? [...other_applets, applet_id] : other_applets;
    });
    setRestartOfferId(needs_restart ? applet_id : undefined);
    setStatus(
      needs_restart
        ? `${name} ${version} installed. Restart to use it.`
        : `${name} ${version} installed.`,
    );
    if (installOpen) onInstallClose();
    try {
      await refresh();
    } catch (error) {
      setLoadError(`Could not refresh the library. ${message(error)}`);
    }
  };
  const openApplet = (applet: InstalledApplet, restart = false) => {
    if (installOpen || pending.current || (action && !restart)) return;
    if (restart) setAction(undefined);
    else setLaunchingId(applet.applet_id);
    void run(
      async () => {
        const open = restart ? restartApplet : launchApplet;
        const session = await open(applet.applet_id);
        if (restart) {
          setRestartRequired((current) => current.filter((id) => id !== applet.applet_id));
          setRestartOfferId((current) => (current === applet.applet_id ? undefined : current));
        }
        setStatus('');
        onLaunch(session);
      },
      `${restart ? 'Restarting' : 'Opening'} ${applet.name}…`,
    );
  };
  const showAction = (target: AppletAction) => {
    if (action || installOpen || pending.current) return;
    setStatus('');
    setFailed(false);
    setAction(target);
  };
  const remove = (target: AppletAction) =>
    void run(async () => {
      await removeApplet(target.applet_id);
      setApplets((current) => current.filter((item) => item.applet_id !== target.applet_id));
      setUnavailable((current) => current.filter((item) => item.applet_id !== target.applet_id));
      setRestartRequired((current) => current.filter((id) => id !== target.applet_id));
      setRestartOfferId((current) => (current === target.applet_id ? undefined : current));
      restore_library_focus.current = true;
      setAction(undefined);
      try {
        await refresh();
      } catch (error) {
        throw new Error(`${target.name} removed. Could not refresh the library. ${message(error)}`);
      }
      setStatus(`${target.name} removed.`);
    }, `Removing ${target.name}…`);
  return (
    <section
      ref={library}
      className="library"
      aria-label="Your applets"
      aria-busy={loading || busy}
    >
      {loading && (
        <p className="status" role="status">
          Loading applets…
        </p>
      )}
      <div className="applet-grid">
        {applets.map((applet) => (
          <AppletTile
            key={applet.applet_id}
            applet={applet}
            launching={launching_id === applet.applet_id}
            disabled={busy}
            restart_required={restart_required.includes(applet.applet_id)}
            onActions={() =>
              showAction({ applet_id: applet.applet_id, name: applet.name, panel: 'actions' })
            }
            onOpen={() => openApplet(applet)}
          />
        ))}
      </div>
      {unavailable.length > 0 && (
        <aside aria-label="Unavailable applets">
          <h2>Unavailable applets</h2>
          {unavailable.map((applet) => (
            <div className="unavailable-applet" key={applet.applet_id}>
              <p className="status error">
                <strong>{applet.applet_id}</strong>: {applet.error}
              </p>
              <button
                className="danger"
                disabled={busy}
                aria-label={`Remove ${applet.applet_id}`}
                onClick={(event) => {
                  event.currentTarget.focus({ preventScroll: true });
                  showAction({
                    applet_id: applet.applet_id,
                    name: applet.applet_id,
                    panel: 'remove',
                  });
                }}
              >
                Remove
              </button>
            </div>
          ))}
          <p className="status">
            These applets cannot run in this version of Apell. Their packages and saved data are
            kept.
          </p>
        </aside>
      )}
      {!loading && !load_error && !applets.length && (
        <div className="empty-library">
          <p>{unavailable.length ? 'Add another applet.' : 'Add your first applet.'}</p>
          <button className="primary" disabled={busy} onClick={onInstallOpen}>
            Add applet
          </button>
        </div>
      )}
      {!installOpen && !action && load_error && (
        <p className="status library-status error" role="alert">
          {load_error}
        </p>
      )}
      {!installOpen && !action && status && (
        <p
          className={`status library-status${failed ? ' error' : ''}`}
          role={failed ? 'alert' : 'status'}
        >
          {status}
        </p>
      )}
      {!installOpen && !action && restart_offer && (
        <div className="button-row library-restart">
          <button
            className="primary"
            disabled={busy}
            onClick={() => openApplet(restart_offer, true)}
          >
            Restart {restart_offer.name}
          </button>
        </div>
      )}
      {action?.panel === 'info' && action_applet ? (
        <AppletInfoSheet
          key={action_applet.applet_id}
          applet={action_applet}
          onClose={() => setAction(undefined)}
          onInstalled={(result) => void installed(result)}
          onRestart={(applet) => openApplet(applet, true)}
        />
      ) : (
        action && (
          <Sheet
            key={action.panel}
            open
            title={action.panel === 'remove' ? `Remove ${action.name}?` : action.name}
            dismissible={!busy}
            onClose={() => setAction(undefined)}
          >
            {action.panel === 'remove' ? (
              <>
                <p>
                  Its saved data stays on this device. Reinstall from the same source to use it
                  again.
                </p>
                <div className="button-row">
                  <button disabled={busy} onClick={() => setAction(undefined)}>
                    Cancel
                  </button>
                  <button className="danger" disabled={busy} onClick={() => remove(action)}>
                    {busy ? 'Removing…' : 'Remove'}
                  </button>
                </div>
                {failed && (
                  <p className="status error" role="alert">
                    {status}
                  </p>
                )}
              </>
            ) : (
              <div className="applet-menu-actions">
                {action_applet && (
                  <button
                    className="applet-menu-action"
                    onClick={() => setAction({ ...action, panel: 'info' })}
                  >
                    <Icon name="info" />
                    Applet info
                  </button>
                )}
                {action_applet && restart_required.includes(action_applet.applet_id) && (
                  <button
                    className="applet-menu-action"
                    onClick={() => openApplet(action_applet, true)}
                  >
                    <Icon name="reload" />
                    Restart applet
                  </button>
                )}
                <button
                  className="applet-menu-action danger"
                  onClick={() => setAction({ ...action, panel: 'remove' })}
                >
                  <Icon name="trash" />
                  Remove applet
                </button>
              </div>
            )}
          </Sheet>
        )
      )}
      <InstallSheet
        open={installOpen}
        onClose={onInstallClose}
        busy={busy || loading}
        hasSample={applets.some((applet) => applet.applet_id === 'chatbot')}
        applets={applets}
        status={loading ? 'Loading applets…' : status || load_error}
        failed={status ? failed : !!load_error}
        onSample={() =>
          void run(async () => {
            const { sample_base64 } = await import('../generated/chatbot');
            await installed(await installBundle({ bundle_base64: sample_base64 }));
          }, 'Installing Companion…')
        }
        onFile={(file) =>
          void run(async () => {
            if (file.size > 24 * 1024 * 1024) throw new Error('Bundle exceeds 24 MiB.');
            const bytes = new Uint8Array(await file.arrayBuffer());
            let binary = '';
            for (let offset = 0; offset < bytes.length; offset += 32768)
              binary += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
            await installed(await installBundle({ bundle_base64: btoa(binary) }));
          }, 'Installing applet…')
        }
        onGitHub={(release, asset_name) =>
          void run(async () => {
            await installed(
              await installBundle({
                repository: release.repository,
                release_tag: release.release_tag,
                release_id: release.release_id,
                asset_id: release.assets.find((asset) => asset.name === asset_name)!.asset_id,
                asset_name,
              }),
            );
          }, 'Downloading applet…')
        }
        onUpdate={(applet_id) =>
          void run(async () => {
            await installed(await updateApplet(applet_id));
          }, 'Checking GitHub release…')
        }
      />
    </section>
  );
}
