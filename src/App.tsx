import { useState } from 'react';
import { Library } from './pages/Library';
import { Settings } from './pages/Settings';
import { AppletView } from './pages/AppletView';
import { closeApplet, nativeAvailable } from './lib/tauri';
import type { AppletSession } from './lib/types';
import { Icon } from './components/Icon';
import { useViewportHeight } from './lib/useViewportHeight';

export default function App() {
  useViewportHeight();
  const [tab, setTab] = useState<'library' | 'settings'>('library');
  const [install_open, setInstallOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [session, setSession] = useState<AppletSession | null>(null);
  if (session)
    return (
      <AppletView
        session={session}
        onClose={async () => {
          await closeApplet(session.session_id);
          setSession(null);
        }}
      />
    );
  return (
    <div className="shell">
      <header className="shell-header">
        <div className="header-title">
          {tab === 'settings' && (
            <button
              className="icon-button"
              aria-label="Back to applets"
              onClick={() => setTab('library')}
            >
              <Icon name="back" />
            </button>
          )}
          <h1>{tab === 'library' ? 'Apell' : 'Settings'}</h1>
        </div>
        {tab === 'library' && (
          <nav aria-label="Library actions">
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
        ) : (
          <Settings />
        )}
      </main>
    </div>
  );
}
