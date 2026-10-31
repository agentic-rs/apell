import { useEffect, useRef, useState } from 'react';
import { installBundle, launchApplet, listApplets, message, onInstallProgress } from '../lib/tauri';
import type { AppletSession, InstalledApplet } from '../lib/types';
import { Icon } from '../components/Icon';
import { InstallSheet } from '../components/InstallSheet';

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
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState('');
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [launching_id, setLaunchingId] = useState('');
  const pending = useRef(false);

  useEffect(() => {
    let disposed = false;
    listApplets()
      .then((items) => {
        if (!disposed) setApplets(items);
      })
      .catch((error) => {
        if (!disposed) {
          setStatus(message(error));
          setFailed(true);
        }
      })
      .finally(() => {
        if (!disposed) setLoading(false);
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
  const installed = async () => {
    setApplets(await listApplets());
    setStatus('Applet added.');
    onInstallClose();
  };
  return (
    <section className="library" aria-label="Your applets" aria-busy={loading || busy}>
      {loading && (
        <p className="status" role="status">
          Loading applets…
        </p>
      )}
      <div className="applet-grid">
        {applets.map((applet) => (
          <button
            className="applet-icon-button"
            key={applet.applet_id}
            disabled={busy}
            aria-label={`Open ${applet.name}`}
            onClick={() => {
              if (pending.current) return;
              setLaunchingId(applet.applet_id);
              void run(
                async () => onLaunch(await launchApplet(applet.applet_id)),
                `Opening ${applet.name}…`,
              );
            }}
          >
            <span className="applet-icon" aria-hidden="true">
              {launching_id === applet.applet_id ? (
                <span className="spinner" />
              ) : applet.applet_id === 'chatbot' ? (
                <Icon name="chat" />
              ) : (
                <span className="applet-initial">
                  {Array.from(applet.name.trim())[0]?.toLocaleUpperCase() || 'a'}
                </span>
              )}
            </span>
            <span className="applet-name">{applet.name}</span>
          </button>
        ))}
      </div>
      {!loading && !applets.length && (
        <div className="empty-library">
          <p>Add your first applet.</p>
          <button className="primary" onClick={onInstallOpen}>
            Add applet
          </button>
        </div>
      )}
      {!installOpen && status && (
        <p
          className={`status library-status${failed ? ' error' : ''}`}
          role={failed ? 'alert' : 'status'}
        >
          {status}
        </p>
      )}
      <InstallSheet
        open={installOpen}
        onClose={onInstallClose}
        busy={busy || loading}
        hasSample={applets.some((applet) => applet.applet_id === 'chatbot')}
        status={loading ? 'Loading applets…' : status}
        failed={failed}
        onSample={() =>
          void run(async () => {
            const { default: bundle } = await import('../generated/chatbot.apell.json?raw');
            await installBundle({ bundle });
            await installed();
          }, 'Installing Companion…')
        }
        onFile={(file) =>
          void run(async () => {
            if (file.size > 24 * 1024 * 1024) throw new Error('Bundle exceeds 24 MiB.');
            await installBundle({ bundle: await file.text() });
            await installed();
          }, 'Installing applet…')
        }
        onUrl={(url, sha256) =>
          void run(async () => {
            await installBundle({ url, sha256 });
            await installed();
          }, 'Downloading applet…')
        }
      />
    </section>
  );
}
