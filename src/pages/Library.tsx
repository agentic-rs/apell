import { useEffect, useState } from 'react';
import { installBundle, launchApplet, listApplets, message, onInstallProgress } from '../lib/tauri';
import type { AppletSession, InstalledApplet } from '../lib/types';

async function checksum(text: string): Promise<string> {
  const hash = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export function Library({ onLaunch }: { onLaunch: (session: AppletSession) => void }) {
  const [applets, setApplets] = useState<InstalledApplet[]>([]);
  const [url, setUrl] = useState('');
  const [sha256, setSha256] = useState('');
  const [status, setStatus] = useState('Loading library…');
  const [busy, setBusy] = useState(false);
  const refresh = async () => {
    setApplets(await listApplets());
    setStatus('');
  };
  useEffect(() => {
    refresh().catch((error) => setStatus(message(error)));
  }, []);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    onInstallProgress((event) =>
      setStatus(
        `Downloading… ${Math.round(event.received_bytes / 1024)} KiB${event.total_bytes ? ` of ${Math.round(event.total_bytes / 1024)} KiB` : ''}`,
      ),
    )
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
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setStatus('Working…');
    try {
      await action();
    } catch (error) {
      setStatus(message(error));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <section className="hero">
        <div>
          <span className="eyebrow">YOUR PERSONAL APPLET SPACE</span>
          <h1>
            Small apps.
            <br />
            <span>A shared foundation.</span>
          </h1>
          <p>
            Your apps, together. Native storage and one model connection, ready whenever you are.
          </p>
        </div>
        <div className="orb" aria-hidden="true">
          <div className="orb-core">
            a<span>·</span>
          </div>
          <span className="orb-tag">LOCAL BY DEFAULT</span>
        </div>
      </section>
      <section className="library-section">
        <div className="section-heading">
          <h2>
            Your applets <span>{applets.length.toString().padStart(2, '0')}</span>
          </h2>
          <span className="muted">On this device</span>
        </div>
        <div className="applet-grid">
          {applets.map((applet) => (
            <button
              className="applet-card"
              key={applet.applet_id}
              disabled={busy}
              onClick={() => run(async () => onLaunch(await launchApplet(applet.applet_id)))}
            >
              <div className="card-top">
                <span className="chat-icon">↗</span>
                <span className="pill">INSTALLED</span>
              </div>
              <h3>{applet.name}</h3>
              <p>
                {applet.applet_id === 'chatbot'
                  ? 'A conversation, backed by Rust WASM and native SQLite.'
                  : applet.applet_id}
              </p>
              <div className="card-bottom">
                <span>v{applet.version}</span>
                <span>Open applet ↗</span>
              </div>
            </button>
          ))}
          {!applets.some((applet) => applet.applet_id === 'chatbot') && (
            <article className="applet-card sample">
              <div className="card-top">
                <span className="chat-icon">✳</span>
                <span className="pill">START HERE</span>
              </div>
              <h3>Chatbot</h3>
              <p>A sample applet with saved conversations and streaming replies.</p>
              <button
                className="primary"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    const { default: sample_bundle } =
                      await import('../generated/chatbot.apell.json?raw');
                    await installBundle({
                      bundle: sample_bundle,
                      sha256: await checksum(sample_bundle),
                    });
                    await refresh();
                  })
                }
              >
                Install sample ↗
              </button>
            </article>
          )}
        </div>
      </section>
      <section className="install-panel">
        <div>
          <span className="eyebrow">BRING YOUR OWN</span>
          <h2>Install an applet</h2>
          <p className="muted">
            Use a bundle URL and its SHA-256 checksum, or choose a local bundle.
          </p>
          <label className="file-button">
            Choose bundle
            <input
              type="file"
              accept=".json,.apell"
              disabled={busy}
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (!file) return;
                run(async () => {
                  if (file.size > 24 * 1024 * 1024) throw new Error('Bundle exceeds 24 MiB.');
                  const bundle = await file.text();
                  await installBundle({ bundle, sha256: await checksum(bundle) });
                  await refresh();
                });
                event.target.value = '';
              }}
            />
          </label>
        </div>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            run(async () => {
              await installBundle({ url, sha256 });
              await refresh();
              setUrl('');
              setSha256('');
            });
          }}
        >
          <label>
            Bundle URL
            <input
              type="url"
              required
              value={url}
              placeholder="https://example.com/chatbot.apell.json"
              onChange={(event) => setUrl(event.target.value)}
            />
          </label>
          <label>
            SHA-256
            <input
              required
              pattern="[a-fA-F0-9]{64}"
              value={sha256}
              placeholder="64-character checksum"
              onChange={(event) => setSha256(event.target.value.trim())}
            />
          </label>
          <button disabled={busy}>Download & install ↓</button>
        </form>
      </section>
      <p className="status" role="status">
        {status}
      </p>
    </>
  );
}
