import { useEffect, useState } from 'react';
import { getSettings, message, saveSettings } from '../lib/tauri';

export function Settings() {
  const [mode, setMode] = useState('demo');
  const [base_url, setBaseUrl] = useState('');
  const [model_id, setModelId] = useState('');
  const [api_key, setApiKey] = useState('');
  const [has_key, setHasKey] = useState(false);
  const [clear_key, setClearKey] = useState(false);
  const [status, setStatus] = useState('Loading settings…');
  const [busy, setBusy] = useState(true);
  useEffect(() => {
    getSettings()
      .then((settings) => {
        setMode(settings.base_url === 'demo' ? 'demo' : 'provider');
        setBaseUrl(settings.base_url === 'demo' ? '' : settings.base_url);
        setModelId(settings.base_url === 'demo' ? '' : settings.model_id);
        setHasKey(settings.has_api_key);
        setStatus('');
      })
      .catch((error) => setStatus(message(error)))
      .finally(() => setBusy(false));
  }, []);
  return (
    <section className="settings panel">
      <span className="eyebrow">SHARED SERVICES</span>
      <h2>
        One connection.
        <br />
        Every applet.
      </h2>
      <p className="muted">
        Choose a model here. Applets send messages through Apell without handling your provider key.
      </p>
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          setBusy(true);
          setStatus('Saving…');
          try {
            await saveSettings(
              {
                base_url: mode === 'demo' ? 'demo' : base_url.trim(),
                model_id: mode === 'demo' ? 'local-demo' : model_id.trim(),
              },
              clear_key ? '' : api_key || null,
            );
            setApiKey('');
            setClearKey(false);
            setStatus('Settings saved. New requests use this connection.');
            setHasKey((await getSettings()).has_api_key);
          } catch (error) {
            setStatus(message(error));
          } finally {
            setBusy(false);
          }
        }}
      >
        <label>
          Connection
          <select value={mode} onChange={(event) => setMode(event.target.value)}>
            <option value="demo">Local demo · no API key</option>
            <option value="provider">OpenAI-compatible provider</option>
          </select>
        </label>
        {mode === 'demo' ? (
          <div className="notice">
            Demo mode returns a scripted response to test streaming and persistence. It does not
            call a language model.
          </div>
        ) : (
          <>
            <label>
              API base URL
              <input
                type="url"
                required
                placeholder="https://api.example.com/v1"
                value={base_url}
                onChange={(event) => {
                  setBaseUrl(event.target.value);
                  setHasKey(false);
                }}
              />
            </label>
            <label>
              Model ID
              <input
                required
                placeholder="Your provider’s model ID"
                value={model_id}
                onChange={(event) => setModelId(event.target.value)}
              />
            </label>
            <label>
              API key
              <input
                type="password"
                autoComplete="off"
                placeholder={
                  has_key
                    ? 'Saved securely · leave blank to keep'
                    : 'Enter a key if your provider requires one'
                }
                value={api_key}
                onChange={(event) => setApiKey(event.target.value)}
              />
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={clear_key}
                onChange={(event) => setClearKey(event.target.checked)}
              />{' '}
              Remove saved key for this endpoint
            </label>
          </>
        )}
        <button className="primary" disabled={busy}>
          Save connection
        </button>
        <p role="status" className="status">
          {status}
        </p>
      </form>
    </section>
  );
}
