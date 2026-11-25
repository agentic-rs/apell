import { useEffect, useRef, useState, type MouseEvent } from 'react';
import { activateConnection, getSettings, message, onLlmSettingsChanged } from '../lib/tauri';
import type { LlmConnection, LlmSettings } from '../lib/types';
import { ConnectionEditor } from '../components/providers/ConnectionEditor';
import { OperationScope } from '../components/providers/state';
import { Icon } from '../components/Icon';
import { AccountSync } from '../components/AccountSync';
import { AppletPermissions } from '../components/AppletPermissions';
import { SettingsTransfer } from '../components/SettingsTransfer';

const settings_sections = [
  { id: 'settings-connections', label: 'Connections' },
  { id: 'settings-account', label: 'Account' },
  { id: 'settings-backup', label: 'Backup' },
  { id: 'settings-permissions', label: 'Permissions' },
];

function jumpToSection(event: MouseEvent<HTMLAnchorElement>) {
  const heading = document.getElementById(event.currentTarget.hash.slice(1));
  const nav = event.currentTarget.closest('nav');
  if (!heading || !nav) return;
  event.preventDefault();
  const pinned_top = Number.parseFloat(window.getComputedStyle(nav).top);
  heading.focus({ preventScroll: true });
  window.scrollTo({
    top: window.scrollY + heading.getBoundingClientRect().top - nav.offsetHeight - pinned_top - 12,
  });
}

export function Settings() {
  const [settings, setSettings] = useState<LlmSettings>();
  const [editor, setEditor] = useState<{ connection?: LlmConnection }>();
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [reload_requested, setReloadRequested] = useState(false);
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
    setBusy(false);
    void load();
    return () => scope.current.cancel();
  }, []);
  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void onLlmSettingsChanged(() => {
      if (active) setReloadRequested(true);
    }).then(
      (unlisten) => {
        if (active) unsubscribe = unlisten;
        else unlisten();
      },
      () => {},
    );
    return () => {
      active = false;
      unsubscribe?.();
    };
  }, []);
  useEffect(() => {
    if (reload_requested && !editor && !loading && !busy) {
      setReloadRequested(false);
      void load();
    }
  }, [reload_requested, editor, loading, busy]);

  const editing_disabled = busy || loading || syncing;

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
                    ? null
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
    <section
      className="settings panel"
      aria-labelledby="settings-connections"
      aria-busy={editing_disabled}
    >
      <div className="settings-heading">
        <h2 id="settings-connections" className="settings-section-title" tabIndex={-1}>
          Connections
        </h2>
        <button
          type="button"
          className="icon-button"
          aria-label="Add connection"
          disabled={editing_disabled || !settings?.providers.length}
          onClick={() => setEditor({})}
        >
          <Icon name="add" />
        </button>
      </div>
      {settings && (
        <p>
          {!settings.connections.length
            ? 'Add a connection to use a model in your applets.'
            : settings.active_connection_id === null
              ? 'Choose a connection to set the default model for your applets.'
              : 'The active model is shared by your applets. Credentials stay in Apell.'}
        </p>
      )}
      {loading && !settings && (
        <p className="status connection-status" role="status">
          Loading connections…
        </p>
      )}
      {!!settings?.connections.length && (
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
                  disabled={editing_disabled}
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
                    disabled={editing_disabled}
                    aria-label={`${credential_action} for ${connection.name}`}
                    onClick={editConnection}
                  >
                    {credential_action}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="connection-use"
                    disabled={editing_disabled || !connection.model_id}
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
      {settings && (
        <button
          type="button"
          className="add-connection"
          disabled={editing_disabled}
          onClick={() => setEditor({})}
        >
          <Icon name="add" /> Add connection
        </button>
      )}
    </section>
  );

  return (
    <>
      <nav className="settings-shortcuts" aria-label="Settings sections">
        {settings_sections.map(({ id, label }) => (
          <a key={id} href={`#${id}`} onClick={jumpToSection}>
            {label}
          </a>
        ))}
      </nav>
      {connections}
      <AccountSync
        providers={settings?.providers ?? []}
        disabled={editing_disabled || !settings}
        onApplied={() => setReloadRequested(true)}
        onSyncBusyChange={setSyncing}
      />
      <SettingsTransfer
        connections={settings?.connections ?? []}
        providers={settings?.providers ?? []}
        disabled={editing_disabled || !settings}
        onImported={() => void load()}
      />
      <AppletPermissions />
    </>
  );
}
