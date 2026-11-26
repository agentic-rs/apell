import { Activity, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useBackAction } from '../lib/backNavigation';
import {
  activateConnection,
  getSettings,
  message,
  onAccountChanged,
  onLlmSettingsChanged,
  saved_connection_changed,
} from '../lib/tauri';
import { connectionReadiness } from '../lib/modelChoice';
import type { LlmConnection, LlmSettings } from '../lib/types';
import { ConnectionEditor } from '../components/providers/ConnectionEditor';
import { SavedConnectionDetails } from '../components/providers/SavedConnectionDetails';
import { OperationScope } from '../components/providers/state';
import { Icon } from '../components/Icon';
import { AccountSync } from '../components/AccountSync';
import { AppletPermissions } from '../components/AppletPermissions';
import { SettingsTransfer } from '../components/SettingsTransfer';
import { SettingsMenu, type SettingsSection } from '../components/SettingsMenu';
import { DefaultModelSettings } from '../components/DefaultModelSettings';

interface AccountConnectionVisit {
  trigger: HTMLButtonElement;
  scroll: number;
  pending_connection_id?: string;
}

export function Settings() {
  const [section, setSection] = useState<SettingsSection | null>(null);
  const [visited_permissions, setVisitedPermissions] = useState(false);
  const [settings, setSettings] = useState<LlmSettings>();
  const [editor, setEditor] = useState<{ connection?: LlmConnection; from_account?: boolean }>();
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [reload_requested, setReloadRequested] = useState(false);
  const [load_error, setLoadError] = useState('');
  const [status, setStatus] = useState('');
  const scope = useRef(new OperationScope());
  const settings_change = useRef(0);
  const previous_section = useRef<SettingsSection | null>(null);
  const menu_scroll = useRef(0);
  const account_visit = useRef<AccountConnectionVisit>(undefined);
  const account_return = useRef<AccountConnectionVisit>(undefined);
  const editor_return = useRef<{ id: string; scroll: number }>(undefined);

  const invalidateSettings = () => {
    settings_change.current += 1;
    setReloadRequested(true);
  };

  const returnToAccount = () => {
    account_return.current = account_visit.current;
    account_visit.current = undefined;
    setSection('account');
  };
  useBackAction(10, section !== null, () => {
    if (busy || syncing) return;
    setStatus('');
    if (section === 'connections' && account_visit.current) returnToAccount();
    else setSection(null);
  });
  useLayoutEffect(() => {
    const previous = previous_section.current;
    if (previous === section) return;
    previous_section.current = section;
    const target = section ? `settings-${section}` : `settings-open-${previous}`;
    const returning = section === 'account' ? account_return.current : undefined;
    account_return.current = undefined;
    const trigger = returning?.trigger;
    const focus =
      trigger?.isConnected && !trigger.disabled ? trigger : document.getElementById(target);
    window.scrollTo(0, returning?.scroll ?? (section ? 0 : menu_scroll.current));
    focus?.focus({ preventScroll: true });
  }, [section]);
  useLayoutEffect(() => {
    if (editor) {
      window.scrollTo(0, 0);
      document.getElementById('settings-connection-editor')?.focus({ preventScroll: true });
    }
  }, [editor]);
  useLayoutEffect(() => {
    if (editor || loading || reload_requested || !editor_return.current) return;
    const { id, scroll } = editor_return.current;
    editor_return.current = undefined;
    const row = document.getElementById(id);
    if (row instanceof HTMLButtonElement && !row.disabled) {
      window.scrollTo(0, scroll);
      row.focus({ preventScroll: true });
    } else {
      document.getElementById(`settings-${section}`)?.focus();
    }
  }, [editor, loading, reload_requested, section]);

  const load = async () => {
    const operation = scope.current.start();
    if (!operation) return;
    const visit = account_visit.current;
    const requested_id = visit?.pending_connection_id;
    const observed_change = settings_change.current;
    const isCurrentVisit = () =>
      visit === account_visit.current && observed_change === settings_change.current;
    setLoading(true);
    setLoadError('');
    try {
      const result = await getSettings();
      if (!operation.isCurrent()) return;
      setSettings(result);
      if (requested_id && isCurrentVisit()) {
        const connection = result.connections.find(
          ({ connection_id }) => connection_id === requested_id,
        );
        if (connection) setEditor({ connection, from_account: true });
        else setStatus('This connection is no longer saved on this device.');
      }
    } catch (error) {
      if (operation.isCurrent()) setLoadError(message(error));
    } finally {
      if (operation.isCurrent()) {
        if (visit && isCurrentVisit()) visit.pending_connection_id = undefined;
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
    const refresh = () => {
      if (active) invalidateSettings();
    };
    const unsubscribe = [onLlmSettingsChanged(refresh), onAccountChanged(refresh)].map(
      (subscription) => subscription.catch(() => undefined),
    );
    return () => {
      active = false;
      for (const stop of unsubscribe) void stop.then((unlisten) => unlisten?.());
    };
  }, []);
  useEffect(() => {
    if (reload_requested && !editor && !loading && !busy) {
      setReloadRequested(false);
      void load();
    }
  }, [reload_requested, editor, loading, busy]);

  const settings_mutation_pending = busy || syncing;
  const editing_disabled = loading || reload_requested || settings_mutation_pending || !!load_error;

  const editConnection = (connection: LlmConnection | undefined, trigger_id: string) => {
    editor_return.current = { id: trigger_id, scroll: window.scrollY };
    setEditor({ connection });
    setStatus('');
  };
  const setActive = (connection_id: string) =>
    setSettings(
      (current) =>
        current && {
          ...current,
          active_connection_id: connection_id,
          pending_preferred_connection_id: null,
        },
    );
  const activate = async (connection: LlmConnection) => {
    const operation = scope.current.start();
    if (!operation) return;
    setBusy(true);
    setStatus('');
    try {
      await activateConnection(connection);
      if (operation.isCurrent()) {
        setActive(connection.connection_id);
        document.getElementById('settings-default-model')?.focus({ preventScroll: true });
      }
    } catch (error) {
      if (operation.isCurrent()) {
        const detail = message(error);
        if (detail === saved_connection_changed) {
          setStatus('This connection changed. Review the current list before choosing a default.');
          setReloadRequested(true);
          document.getElementById('settings-default-model')?.focus();
        } else setStatus(detail);
      }
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setBusy(false);
      }
    }
  };

  const connections_editor = editor && settings && (
    <ConnectionEditor
      connection={editor.connection}
      back_label={
        editor.from_account
          ? 'Apell Account'
          : section === 'default-model'
            ? 'Default model'
            : undefined
      }
      providers={settings.providers}
      active_connection_id={settings.active_connection_id}
      onBack={(refresh) => {
        setEditor(undefined);
        setStatus('');
        if (editor.from_account) returnToAccount();
        if (refresh) setReloadRequested(true);
      }}
      onSaved={(connection, previous_id) => {
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
        });
        setReloadRequested(true);
      }}
      onActivated={setActive}
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
              pending_preferred_connection_id:
                current.pending_preferred_connection_id === connection_id
                  ? null
                  : current.pending_preferred_connection_id,
            },
        );
        setEditor(undefined);
        setStatus('');
        if (editor.from_account) returnToAccount();
        setReloadRequested(true);
      }}
    />
  );

  const connections = (
    <section
      className="settings panel"
      aria-labelledby="settings-connections"
      aria-busy={editing_disabled}
    >
      <h2 id="settings-connections" className="settings-section-title" tabIndex={-1}>
        Model connections
      </h2>
      {settings && !settings.connections.length && <p>Add a provider connection to use models.</p>}
      {!!account_visit.current?.pending_connection_id ? (
        <p className="status connection-status" role="status">
          Checking connection…
        </p>
      ) : (
        loading &&
        !settings && (
          <p className="status connection-status" role="status">
            Loading connections…
          </p>
        )
      )}
      {status && (
        <p className="status connection-status error" role="alert">
          {status}
        </p>
      )}
      {!!settings?.connections.length && (
        <div className="connection-list">
          {settings.connections.map((connection) => {
            const active = settings.active_connection_id === connection.connection_id;
            const readiness = connectionReadiness(connection, settings.providers);
            const id = `settings-connection-${connection.connection_id}`;
            return (
              <div
                className={`connection-row${active ? ' active' : ''}`}
                key={connection.connection_id}
              >
                <button
                  type="button"
                  id={id}
                  className="connection-detail"
                  disabled={editing_disabled}
                  aria-label={`Edit ${connection.name}`}
                  onClick={() => editConnection(connection, id)}
                >
                  <SavedConnectionDetails connection={connection} readiness={readiness} />
                </button>
                {active && (
                  <span className="connection-badge">
                    {readiness.state === 'ready' ? 'Default' : 'Selected'}
                  </span>
                )}
              </div>
            );
          })}
        </div>
      )}
      {load_error && (
        <p className="status connection-status error" role="alert">
          {load_error}
        </p>
      )}
      {!loading && (!settings || !!load_error) && (
        <button type="button" disabled={settings_mutation_pending} onClick={() => void load()}>
          Try again
        </button>
      )}
      {settings && (
        <button
          type="button"
          className="add-connection"
          id="settings-add-connection"
          disabled={editing_disabled || !settings.providers.length}
          onClick={() => editConnection(undefined, 'settings-add-connection')}
        >
          <Icon name="add" /> Add connection
        </button>
      )}
    </section>
  );

  return (
    <>
      {section === null && (
        <SettingsMenu
          settings={settings}
          loading={loading}
          onOpen={(next) => {
            menu_scroll.current = window.scrollY;
            if (next === 'permissions') setVisitedPermissions(true);
            setSection(next);
          }}
        />
      )}
      {section === 'default-model' &&
        (connections_editor || (
          <DefaultModelSettings
            settings={settings}
            loading={loading}
            disabled={editing_disabled}
            error={status}
            load_error={load_error}
            retry_disabled={settings_mutation_pending}
            onActivate={(connection) => void activate(connection)}
            onEdit={editConnection}
            onRetry={() => void load()}
          />
        ))}
      {section === 'connections' && (connections_editor || connections)}
      <Activity mode={section === 'account' ? 'visible' : 'hidden'}>
        <AccountSync
          providers={settings?.providers ?? []}
          connections={settings?.connections ?? []}
          disabled={settings_mutation_pending}
          onApplied={invalidateSettings}
          onSyncBusyChange={setSyncing}
          onOpenConnections={(trigger, pending_connection_id) => {
            account_visit.current = { trigger, scroll: window.scrollY, pending_connection_id };
            setStatus('');
            setSection('connections');
            if (pending_connection_id) setReloadRequested(true);
          }}
        />
      </Activity>
      {section === 'backup' && (
        <SettingsTransfer
          connections={settings?.connections ?? []}
          connections_status={loading ? 'loading' : load_error || !settings ? 'failed' : 'ready'}
          providers={settings?.providers ?? []}
          disabled={settings_mutation_pending}
          onRetry={() => void load()}
          onImported={() => setReloadRequested(true)}
        />
      )}
      {visited_permissions && (
        <Activity mode={section === 'permissions' ? 'visible' : 'hidden'}>
          <AppletPermissions />
        </Activity>
      )}
    </>
  );
}
