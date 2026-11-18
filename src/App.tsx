import { useEffect, useState } from 'react';
import { Library } from './pages/Library';
import { Settings } from './pages/Settings';
import { Store } from './pages/Store';
import { AppletView } from './pages/AppletView';
import { closeApplet, reloadApplet, currentApplet, nativeAvailable } from './lib/tauri';
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

function Shell() {
  useViewportHeight();
  const [tab, setTab] = useState<'library' | 'store' | 'settings'>('library');
  const [install_open, setInstallOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [session, setSession] = useState<AppletSession | null>(null);
  const [starting, setStarting] = useState(nativeAvailable());
  const back = useBackNavigation();
  useBackAction(0, starting || tab !== 'library' || busy, () => {
    if (!starting && !busy) setTab('library');
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
  if (session)
    return (
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
      />
    );
  return (
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
              onClick={() => setTab('store')}
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
              onClick={() => setTab('settings')}
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
        ) : tab === 'library' ? (
          <Library
            onLaunch={setSession}
            onBusyChange={setBusy}
            installOpen={install_open}
            onInstallOpen={() => setInstallOpen(true)}
            onInstallClose={() => setInstallOpen(false)}
          />
        ) : tab === 'store' ? (
          <Store onLaunch={setSession} onBusyChange={setBusy} />
        ) : (
          <Settings />
        )}
      </main>
    </div>
  );
}
