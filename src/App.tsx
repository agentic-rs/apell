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
  const back = useBackNavigation();
  const scroll = useRef<Record<ShellTab, number>>({ library: 0, store: 0, settings: 0 });
  const applet_open = session !== null;
  const changeTab = (next: ShellTab) => {
    if (next === tab) return;
    scroll.current[tab] = window.scrollY;
    setVisited((current) => (current[next] ? current : { ...current, [next]: true }));
    setTab(next);
  };
  const launch = (next: AppletSession) => {
    scroll.current[tab] = window.scrollY;
    setSession(next);
  };
  useLayoutEffect(() => {
    if (!starting && !applet_open) window.scrollTo(0, scroll.current[tab]);
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
                  <Library
                    onLaunch={launch}
                    onBusyChange={setBusy}
                    installOpen={install_open}
                    onInstallOpen={() => setInstallOpen(true)}
                    onInstallClose={() => setInstallOpen(false)}
                  />
                </Activity>
                {visited.store && (
                  <Activity mode={tab === 'store' ? 'visible' : 'hidden'}>
                    <Store onLaunch={launch} onBusyChange={setBusy} />
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
