import { Activity, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Library } from './pages/Library';
import { Settings } from './pages/Settings';
import { Store } from './pages/Store';
import { AppletView } from './pages/AppletView';
import {
  closeApplet,
  reloadApplet,
  restartApplet,
  currentApplet,
  nativeAvailable,
} from './lib/tauri';
import type { AppletSession } from './lib/types';
import { Icon } from './components/Icon';
import { useStoreDownloads } from './components/store/useStoreDownloads';
import { useViewportHeight } from './lib/useViewportHeight';
import { BackNavigation, useBackAction, useBackNavigation } from './lib/backNavigation';

export default function App() {
  return (
    <BackNavigation>
      <Shell />
    </BackNavigation>
  );
}

type ShellTab = 'library' | 'store' | 'settings';

function Shell() {
  useViewportHeight();
  const [tab, setTab] = useState<ShellTab>('library');
  const [visited, setVisited] = useState({ library: true, store: false, settings: false });
  const [install_open, setInstallOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [session, setSession] = useState<AppletSession | null>(null);
  const [starting, setStarting] = useState(nativeAvailable());
  const downloads = useStoreDownloads();
  const [downloads_request, setDownloadsRequest] = useState(0);
  const back = useBackNavigation();
  const scroll = useRef<Record<ShellTab, number>>({ library: 0, store: 0, settings: 0 });
  const store_button = useRef<HTMLButtonElement>(null);
  const downloads_button = useRef<HTMLButtonElement>(null);
  const restore_download_focus = useRef(false);
  const applet_open = session !== null;
  const changeTab = (next: ShellTab, previous_scroll = window.scrollY) => {
    if (next === tab) return;
    scroll.current[tab] = previous_scroll;
    setVisited((current) => (current[next] ? current : { ...current, [next]: true }));
    setTab(next);
  };
  const launch = (next: AppletSession) => {
    scroll.current[tab] = window.scrollY;
    setSession(next);
  };
  useLayoutEffect(() => {
    if (!starting && !applet_open) window.scrollTo(0, scroll.current[tab]);
    if (!starting && !applet_open && tab === 'library' && restore_download_focus.current) {
      restore_download_focus.current = false;
      (downloads_button.current ?? store_button.current)?.focus({ preventScroll: true });
    }
  }, [starting, applet_open, tab]);
  useBackAction(0, !applet_open && (starting || tab !== 'library' || busy), () => {
    if (!starting && !busy) changeTab('library');
  });
  useEffect(() => {
    if (!nativeAvailable()) return;
    let disposed = false;
    currentApplet()
      .then((running) => {
        if (!disposed) setSession(running);
      })
      .catch(() => {})
      .finally(() => {
        if (!disposed) setStarting(false);
      });
    return () => {
      disposed = true;
    };
  }, []);
  if (starting) return <p className="status">Opening Apell…</p>;
  return (
    <>
      <Activity mode={applet_open ? 'hidden' : 'visible'}>
        <div className="shell">
          <header className="shell-header">
            <div className="header-title">
              {tab !== 'library' && (
                <button
                  className="icon-button"
                  aria-label="Go back"
                  disabled={busy}
                  onClick={() => back?.goBack()}
                >
                  <Icon name="back" />
                </button>
              )}
              <h1>{tab === 'library' ? 'Apell' : tab === 'store' ? 'Store' : 'Settings'}</h1>
            </div>
            {tab === 'library' && (
              <nav aria-label="Library actions">
                <button
                  ref={store_button}
                  className="icon-button"
                  aria-label="Browse store"
                  title="Browse store"
                  disabled={!nativeAvailable() || busy}
                  onClick={() => changeTab('store')}
                >
                  <Icon name="store" />
                </button>
                <button
                  className="icon-button"
                  aria-label="Add applet"
                  title="Add applet"
                  disabled={!nativeAvailable() || busy}
                  onClick={() => setInstallOpen(true)}
                >
                  <Icon name="add" />
                </button>
                <button
                  className="icon-button"
                  aria-label="Settings"
                  title="Settings"
                  disabled={busy}
                  onClick={() => changeTab('settings')}
                >
                  <Icon name="settings" />
                </button>
              </nav>
            )}
          </header>
          <main className="shell-main">
            {!nativeAvailable() ? (
              <section className="panel native-notice">
                <h2>Open Apell’s native app</h2>
                <p>The applet library is available in the installed shell.</p>
              </section>
            ) : (
              <>
                <Activity mode={tab === 'library' ? 'visible' : 'hidden'}>
                  {downloads.entries.length > 0 && (
                    <button
                      ref={downloads_button}
                      type="button"
                      className="library-downloads"
                      disabled={busy}
                      onClick={() => {
                        restore_download_focus.current = true;
                        setDownloadsRequest((request) => request + 1);
                        changeTab('store');
                      }}
                    >
                      <Icon name="download" />
                      <span>
                        {downloads.entries.length} paused{' '}
                        {downloads.entries.length === 1 ? 'download' : 'downloads'}
                      </span>
                      <span className="library-downloads-arrow">
                        <Icon name="back" />
                      </span>
                    </button>
                  )}
                  <Library
                    onLaunch={launch}
                    onBusyChange={setBusy}
                    downloads={downloads}
                    installOpen={install_open}
                    onInstallOpen={() => setInstallOpen(true)}
                    onInstallClose={() => setInstallOpen(false)}
                  />
                </Activity>
                {visited.store && (
                  <Activity mode={tab === 'store' ? 'visible' : 'hidden'}>
                    <Store
                      onLaunch={launch}
                      onBusyChange={setBusy}
                      downloads={downloads}
                      downloads_request={downloads_request}
                      onReturnToLibrary={(store_scroll) => changeTab('library', store_scroll)}
                    />
                  </Activity>
                )}
                {visited.settings && (
                  <Activity mode={tab === 'settings' ? 'visible' : 'hidden'}>
                    <Settings />
                  </Activity>
                )}
              </>
            )}
          </main>
        </div>
      </Activity>
      {session && (
        <AppletView
          key={session.view_id}
          session={session}
          onClose={async () => {
            await closeApplet(session.session_id, session.view_id);
            setSession(null);
          }}
          onReload={async () => {
            setSession(await reloadApplet(session.session_id, session.view_id));
          }}
          onRestart={async () => {
            setSession(await restartApplet(session.manifest.applet_id));
          }}
        />
      )}
    </>
  );
}
