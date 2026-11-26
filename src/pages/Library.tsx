import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  installBundle,
  updateApplet,
  launchApplet,
  listApplets,
  message,
  onAppletInstallationChanged,
  onInstallProgress,
  removeApplet,
  restartApplet,
} from '../lib/tauri';
import { readPackageFile } from '../lib/packageFile';
import type {
  AppletInstallResult,
  AppletListReport,
  AppletSession,
  InstallProgress,
  InstalledApplet,
  StoreSwitchSnapshot,
  UnavailableApplet,
} from '../lib/types';
import { InstallSheet } from '../components/InstallSheet';
import { AppletTile } from '../components/AppletTile';
import { AppletInfoSheet } from '../components/AppletInfoSheet';
import { Icon } from '../components/Icon';
import { RestorePackageSheet } from '../components/RestorePackageSheet';
import { RunningApplets } from '../components/RunningApplets';
import { Sheet } from '../components/Sheet';
import { UnavailableAppletPermissions } from '../components/UnavailableAppletPermissions';
import type { StoreDownloads } from '../components/store/useStoreDownloads';

type AppletAction = {
  applet_id: string;
  name: string;
} & (
  | { panel: 'actions' | 'info' | 'remove' | 'permissions' }
  | { panel: 'restore'; expected: StoreSwitchSnapshot }
);

interface LibraryProps {
  onLaunch: (session: AppletSession) => void;
  onBusyChange: (busy: boolean) => void;
  downloads: StoreDownloads;
  installOpen: boolean;
  onInstallOpen: () => void;
  onInstallClose: () => void;
}

export function Library({
  onLaunch,
  onBusyChange,
  downloads,
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
  const [installation_change, setInstallationChange] = useState(0);
  const pending = useRef(false);
  const progress_filter = useRef<Pick<InstallProgress, 'operation_id'>>(undefined);
  const list_revision = useRef(0);
  const latest_change = useRef(0);
  const checked_change = useRef(0);
  const library = useRef<HTMLElement>(null);
  const library_focus = useRef<{ applet_id?: string }>(undefined);
  const focused_before_report = useRef<{ element: Element; applet_id?: string }>(undefined);
  const restart_offer = applets.find(
    (applet) =>
      applet.applet_id === restart_offer_id && restart_required.includes(applet.applet_id),
  );
  const action_applet = applets.find((applet) => applet.applet_id === action?.applet_id);

  const applyReport = useCallback((report: AppletListReport) => {
    const element = document.activeElement;
    focused_before_report.current =
      element && library.current?.contains(element)
        ? {
            element,
            applet_id: element.closest<HTMLElement>('[data-library-applet]')?.dataset.libraryApplet,
          }
        : undefined;
    setApplets(report.applets);
    setUnavailable(report.unavailable);
    setRestartRequired(report.restart_required ?? []);
    setLoadError('');
  }, []);

  useEffect(() => {
    if (busy || action || installOpen) return;
    const previous = focused_before_report.current;
    focused_before_report.current = undefined;
    const target =
      library_focus.current ??
      (previous && !previous.element.isConnected && document.activeElement === document.body
        ? previous
        : undefined);
    if (!target) return;
    library_focus.current = undefined;
    const { applet_id } = target;
    const applet = applet_id
      ? (document.getElementById(`library-open-${applet_id}`) ??
        document.getElementById(`library-permissions-${applet_id}`))
      : null;
    (applet ?? library.current?.querySelector('button:not(:disabled)') ?? library.current)?.focus({
      preventScroll: true,
    });
  }, [applets, unavailable, busy, action, installOpen]);

  const refresh = useCallback(
    async (accept: () => boolean = () => true) => {
      const revision = ++list_revision.current;
      const change = latest_change.current;
      const isCurrent = () => revision === list_revision.current && accept();
      try {
        const report = await listApplets();
        if (isCurrent()) {
          checked_change.current = change;
          applyReport(report);
          return report;
        }
      } catch (error) {
        if (isCurrent()) throw error;
      } finally {
        if (isCurrent()) setLoading(false);
      }
    },
    [applyReport],
  );

  useEffect(() => {
    let active = true;
    const request = () => {
      if (active) setInstallationChange(++latest_change.current);
    };
    const subscription = onAppletInstallationChanged(request).catch(() => undefined);
    void subscription.then(request);
    return () => {
      active = false;
      list_revision.current++;
      void subscription.then((stop) => stop?.());
    };
  }, []);
  const list_blocked = busy || installOpen || !!action;
  useLayoutEffect(() => {
    // An open sheet keeps its original trigger and review until it closes.
    if (list_blocked || pending.current || installation_change === checked_change.current) return;
    let active = true;
    const accept = () => active && installation_change === latest_change.current;
    void refresh(accept).catch((error) => {
      if (!accept()) return;
      checked_change.current = installation_change;
      setLoadError(message(error));
    });
    return () => {
      active = false;
    };
  }, [installation_change, list_blocked, refresh]);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    onInstallProgress((event) => {
      const filter = progress_filter.current;
      if (!disposed && filter && event.operation_id === filter.operation_id)
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

  const run = async (
    action: () => Promise<void>,
    label: string,
    progress?: Pick<InstallProgress, 'operation_id'>,
  ) => {
    if (pending.current) return;
    pending.current = true;
    progress_filter.current = progress;
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
      progress_filter.current = undefined;
      onBusyChange(false);
      setBusy(false);
      setLaunchingId('');
    }
  };
  const reloadLibrary = () =>
    void run(async () => {
      try {
        await refresh();
      } catch (error) {
        setLoadError(message(error));
      }
      setStatus('');
    }, 'Refreshing library…');
  const installed = async (
    { applet, restart_required: needs_restart }: AppletInstallResult,
    action: 'installed' | 'restored' = 'installed',
  ) => {
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
    const summary = `${name} ${version} ${action}.`;
    setStatus(needs_restart ? `${summary} Restart to use it.` : summary);
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
        let session: AppletSession;
        try {
          session = await open(applet.applet_id);
        } catch (error) {
          const report = await refresh().catch(() => undefined);
          if (report?.unavailable.some(({ applet_id }) => applet_id === applet.applet_id)) {
            setStatus('');
            return;
          }
          throw error;
        }
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
  const restored = async (result: AppletInstallResult) => {
    library_focus.current = { applet_id: result.applet.applet_id };
    setAction(undefined);
    await installed(result, 'restored');
  };
  const childBusyChanged = (busy: boolean) => {
    pending.current = busy;
    setBusy(busy);
    onBusyChange(busy);
  };
  const remove = (target: AppletAction) =>
    void run(async () => {
      await removeApplet(target.applet_id);
      setApplets((current) => current.filter((item) => item.applet_id !== target.applet_id));
      setUnavailable((current) => current.filter((item) => item.applet_id !== target.applet_id));
      setRestartRequired((current) => current.filter((id) => id !== target.applet_id));
      setRestartOfferId((current) => (current === target.applet_id ? undefined : current));
      library_focus.current = {};
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
      tabIndex={-1}
    >
      {loading && (
        <p className="status" role="status">
          Loading applets…
        </p>
      )}
      <RunningApplets
        disabled={busy || installOpen || !!action}
        onBusyChange={childBusyChanged}
        onStopped={({ applet_id }) => {
          setRestartRequired((current) => current.filter((id) => id !== applet_id));
          setRestartOfferId((current) => (current === applet_id ? undefined : current));
          setStatus('');
          setFailed(false);
        }}
        onReturnToLibrary={() => library.current?.focus({ preventScroll: true })}
      />
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
          <div className="unavailable-applets-heading">
            <h2>Unavailable applets</h2>
            <button
              type="button"
              className="icon-button"
              aria-label="Refresh library"
              title="Refresh library"
              disabled={loading || busy}
              onClick={reloadLibrary}
            >
              <Icon name="reload" />
            </button>
          </div>
          {unavailable.map(({ applet_id, error, can_remove, restore_snapshot }) => (
            <div className="unavailable-applet" key={applet_id} data-library-applet={applet_id}>
              <strong>{applet_id}</strong>
              <p>{error}</p>
              <div className="button-row">
                {restore_snapshot && (
                  <button
                    disabled={busy}
                    aria-label={`Restore package for ${applet_id}`}
                    aria-haspopup="dialog"
                    onClick={(event) => {
                      event.currentTarget.focus({ preventScroll: true });
                      showAction({
                        applet_id,
                        name: applet_id,
                        panel: 'restore',
                        expected: restore_snapshot,
                      });
                    }}
                  >
                    Restore package
                  </button>
                )}
                <button
                  type="button"
                  id={`library-permissions-${applet_id}`}
                  disabled={busy}
                  aria-label={`Device permissions for ${applet_id}`}
                  aria-haspopup="dialog"
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true });
                    showAction({ applet_id, name: applet_id, panel: 'permissions' });
                  }}
                >
                  Device permissions
                </button>
                {can_remove && (
                  <button
                    className="danger"
                    disabled={busy}
                    aria-label={`Remove ${applet_id}`}
                    aria-haspopup="dialog"
                    onClick={(event) => {
                      event.currentTarget.focus({ preventScroll: true });
                      showAction({ applet_id, name: applet_id, panel: 'remove' });
                    }}
                  >
                    Remove
                  </button>
                )}
              </div>
            </div>
          ))}
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
        <div>
          <p className="status library-status error" role="alert">
            {load_error}
          </p>
          <button type="button" disabled={loading || busy} onClick={reloadLibrary}>
            Try again
          </button>
        </div>
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
      ) : action?.panel === 'permissions' ? (
        <UnavailableAppletPermissions
          key={action.applet_id}
          applet_id={action.applet_id}
          onClose={(installation_changed) => {
            setAction(undefined);
            if (installation_changed) {
              library_focus.current = { applet_id: action.applet_id };
              reloadLibrary();
            }
          }}
        />
      ) : action?.panel === 'restore' ? (
        <RestorePackageSheet
          key={action.applet_id}
          target={action}
          downloads={downloads}
          onBusyChange={childBusyChanged}
          onClose={() => setAction(undefined)}
          onRestored={restored}
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
            await installed(await installBundle({ bundle_base64: await readPackageFile(file) }));
          }, 'Installing applet…')
        }
        onGitHub={(release, asset_name) =>
          void run(
            async () => {
              await installed(
                await installBundle({
                  repository: release.repository,
                  release_tag: release.release_tag,
                  release_id: release.release_id,
                  asset_id: release.assets.find((asset) => asset.name === asset_name)!.asset_id,
                  asset_name,
                }),
              );
            },
            'Downloading applet…',
            {},
          )
        }
        onUpdate={({ applet_id, sha256, source }) => {
          if (source?.kind !== 'github') return;
          void run(
            async () => {
              try {
                const result = await updateApplet(applet_id, sha256, source);
                if (result.applet.sha256 === sha256)
                  setStatus(`${result.applet.name} is up to date.`);
                else await installed(result);
              } catch (error) {
                await refresh().catch(() => undefined);
                throw error;
              }
            },
            'Checking GitHub release…',
            {},
          );
        }}
      />
    </section>
  );
}
