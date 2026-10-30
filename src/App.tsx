import { useState } from 'react';
import { Library } from './pages/Library';
import { Settings } from './pages/Settings';
import { AppletView } from './pages/AppletView';
import { closeApplet, message, nativeAvailable } from './lib/tauri';
import type { AppletSession } from './lib/types';

export default function App() {
  const [tab, setTab] = useState('library');
  const [session, setSession] = useState<AppletSession | null>(null);
  const [error, setError] = useState('');
  if (session)
    return (
      <>
        <AppletView
          session={session}
          onClose={async () => {
            try {
              await closeApplet(session.session_id);
              setSession(null);
            } catch (error) {
              setError(message(error));
            }
          }}
        />
        {error && (
          <div className="floating-error" role="alert">
            {error}
          </div>
        )}
      </>
    );
  return (
    <div className="shell">
      <header className="shell-header">
        <a className="brand" href="#" onClick={() => setTab('library')}>
          <span className="brand-mark">a</span>apell<span className="brand-dot">/</span>
        </a>
        <nav>
          <button className={tab === 'library' ? 'active' : ''} onClick={() => setTab('library')}>
            Library
          </button>
          <button className={tab === 'settings' ? 'active' : ''} onClick={() => setTab('settings')}>
            Model connection
          </button>
        </nav>
        <span className="device-badge">
          <i /> Personal space
        </span>
      </header>
      <main>
        {!nativeAvailable() ? (
          <section className="panel">
            <h1>Apell needs its native shell.</h1>
            <p>
              Run <code>pnpm desktop:dev</code> to use SQLite, secure credentials, and applets.
            </p>
          </section>
        ) : tab === 'library' ? (
          <Library onLaunch={setSession} />
        ) : (
          <Settings />
        )}
      </main>
      <footer>
        <span>APELL / PERSONAL APPLET HOST</span>
        <span>Local storage · Shared services</span>
      </footer>
    </div>
  );
}
