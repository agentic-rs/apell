import { useEffect, useRef, useState } from 'react';
import { activateConnection, getSettings, message } from '../lib/tauri';
import type { LlmConnection, LlmSettings } from '../lib/types';
import { ConnectionEditor } from '../components/providers/ConnectionEditor';
import { OperationScope } from '../components/providers/state';
import { Icon } from '../components/Icon';
import { AppletPermissions } from '../components/AppletPermissions';
import { SettingsTransfer } from '../components/SettingsTransfer';

export function Settings() {
  const [settings, setSettings] = useState<LlmSettings>();
  const [editor, setEditor] = useState<{ connection?: LlmConnection }>();
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const scope = useRef(new OperationScope());

  const load = async () => {
    const operation = scope.current.start();
    if (!operation) return;
    setLoading(true);
    setStatus('');
    try {
      const result = await getSettings();
      if (operation.isCurrent()) setSettings(result);
    } catch (error) {
      if (operation.isCurrent()) setStatus(message(error));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setLoading(false);
      }
    }
  };
  useEffect(() => {
    void load();
    return () => scope.current.cancel();
  }, []);

  if (editor && settings)
    return (
      <ConnectionEditor
        connection={editor.connection}
        providers={settings.providers}
        active_connection_id={settings.active_connection_id}
        onBack={() => {
          setEditor(undefined);
          setStatus('');
        }}
        onSaved={(connection, previous_id) =>
          setSettings((current) => {
            if (!current) return current;
            return {
              ...current,
              active_connection_id:
                current.active_connection_id === previous_id
                  ? connection.connection_id
                  : current.active_connection_id,
              connections: previous_id
                ? current.connections.map((item) =>
                    item.connection_id === previous_id ? connection : item,
                  )
                : [...current.connections, connection],
            };
          })
        }
        onActivated={(connection_id) =>
          setSettings(
            (current) =>
              current && {
                ...current,
                active_connection_id: connection_id,
              },
          )
        }
        onDeleted={(connection_id) => {
          setSettings(
            (current) =>
              current && {
                ...current,
                connections: current.connections.filter(
                  (item) => item.connection_id !== connection_id,
                ),
                active_connection_id:
                  current.active_connection_id === connection_id
                    ? (current.connections.find((item) => item.provider_id === 'demo')
                        ?.connection_id ?? '')
                    : current.active_connection_id,
              },
          );
          setEditor(undefined);
          setStatus('');
          void load();
        }}
      />
    );

  const connections = (
    <section className="settings panel" aria-busy={loading || busy}>
      <div className="settings-heading">
        <h2>Connections</h2>
        <button
          type="button"
          className="icon-button"
          aria-label="Add connection"
          disabled={loading || busy || !settings?.providers.length}
          onClick={() => setEditor({})}
        >
          <Icon name="add" />
        </button>
      </div>
      <p>One active model, shared by your applets. Credentials stay in Apell.</p>
      {loading && (
        <p className="status connection-status" role="status">
          Loading connections…
        </p>
      )}
      {settings && (
        <div className="connection-list">
          {settings.connections.map((connection) => {
            const active = settings.active_connection_id === connection.connection_id;
            const provider = settings.providers.find(
              (item) => item.provider_id === connection.provider_id,
            );
            const needs_credential =
              connection.credential_status === 'absent' &&
              provider &&
              !provider.auth_methods.includes('none');
            const credential_action = provider?.auth_methods.includes('device_code')
              ? 'Sign in'
              : 'Add API key';
            const editConnection = () => {
              setEditor({ connection });
              setStatus('');
            };
            return (
              <div
                className={`connection-row${active ? ' active' : ''}`}
                key={connection.connection_id}
              >
                <button
                  type="button"
                  className="connection-detail"
                  disabled={busy || loading}
                  aria-label={`Edit ${connection.name}`}
                  onClick={editConnection}
                >
                  <strong>{connection.name}</strong>
                  <span>{connection.model_id || 'Choose a model'}</span>
                  <small>
                    {provider?.name ?? connection.provider_id}
                    {connection.credential_status === 'unavailable'
                      ? ' · Credential unavailable'
                      : ''}
                  </small>
                </button>
                {active ? (
                  <span className="connection-badge">Active</span>
                ) : needs_credential ? (
                  <button
                    type="button"
                    className="connection-use"
                    disabled={busy || loading}
                    aria-label={`${credential_action} for ${connection.name}`}
                    onClick={editConnection}
                  >
                    {credential_action}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="connection-use"
                    disabled={busy || loading || !connection.model_id}
                    aria-label={`Use ${connection.name}`}
                    onClick={async () => {
                      const operation = scope.current.start();
                      if (!operation) return;
                      setBusy(true);
                      setStatus('');
                      try {
                        await activateConnection(connection.connection_id);
                        if (operation.isCurrent())
                          setSettings(
                            (current) =>
                              current && {
                                ...current,
                                active_connection_id: connection.connection_id,
                              },
                          );
                      } catch (error) {
                        if (operation.isCurrent()) setStatus(message(error));
                      } finally {
                        if (operation.isCurrent()) {
                          operation.finish();
                          setBusy(false);
                        }
                      }
                    }}
                  >
                    Use
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}
      {status && (
        <p className="status connection-status error" role="alert">
          {status}
        </p>
      )}
      {!loading && !settings && (
        <button type="button" onClick={() => void load()}>
          Try again
        </button>
      )}
      {!loading && settings && (
        <button
          type="button"
          className="add-connection"
          disabled={busy}
          onClick={() => setEditor({})}
        >
          <Icon name="add" /> Add connection
        </button>
      )}
    </section>
  );

  return (
    <>
      {connections}
      {settings && (
        <SettingsTransfer
          connections={settings.connections}
          providers={settings.providers}
          disabled={busy || loading}
          onImported={() => void load()}
        />
      )}
      <AppletPermissions />
    </>
  );
}
