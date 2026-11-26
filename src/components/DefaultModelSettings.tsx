import {
  connectionReadiness,
  connectionReadinessAction,
  connectionReadinessLabel,
} from '../lib/modelChoice';
import type { LlmConnection, LlmSettings } from '../lib/types';
import { Icon } from './Icon';
import { SavedConnectionDetails } from './providers/SavedConnectionDetails';

export function DefaultModelSettings({
  settings,
  loading,
  disabled,
  error,
  load_error,
  retry_disabled,
  onActivate,
  onEdit,
  onRetry,
}: {
  settings?: LlmSettings;
  loading: boolean;
  disabled: boolean;
  error: string;
  load_error: string;
  retry_disabled: boolean;
  onActivate: (connection: LlmConnection) => void;
  onEdit: (connection: LlmConnection | undefined, trigger_id: string) => void;
  onRetry: () => void;
}) {
  const pending = settings?.connections.find(
    (connection) => connection.connection_id === settings.pending_preferred_connection_id,
  );
  const pending_problem =
    pending &&
    settings &&
    connectionReadinessLabel(connectionReadiness(pending, settings.providers));
  return (
    <section
      className="settings panel"
      aria-labelledby="settings-default-model"
      aria-busy={disabled}
    >
      <h2 className="settings-section-title" id="settings-default-model" tabIndex={-1}>
        Default model
      </h2>
      <p>Applets use this model unless they choose their own.</p>
      {error && (
        <p className="status connection-status error" role="alert">
          {error}
        </p>
      )}
      {loading && !settings && (
        <p className="status" role="status">
          Loading connections…
        </p>
      )}
      {pending && (
        <div className="default-model-pending" role="status">
          <strong>Synced choice pending</strong>
          <p>
            {pending.name}
            {pending_problem ? ` · ${pending_problem}` : ''}
          </p>
          <p>Apell will switch when the connection is ready and sync completes.</p>
        </div>
      )}
      {settings &&
        (settings.connections.length ? (
          <div className="connection-list">
            {settings.connections.map((connection) => {
              const selected = settings.active_connection_id === connection.connection_id;
              const readiness = connectionReadiness(connection, settings.providers);
              const details = (
                <SavedConnectionDetails connection={connection} readiness={readiness} />
              );
              if (selected && readiness.state === 'ready') {
                return (
                  <div className="connection-row active" key={connection.connection_id}>
                    <div className="connection-detail">{details}</div>
                    <span className="connection-badge">Default</span>
                  </div>
                );
              }
              const action = connectionReadinessAction(readiness);
              const id = `default-model-option-${connection.connection_id}`;
              return (
                <button
                  key={connection.connection_id}
                  type="button"
                  id={id}
                  className={`connection-row default-model-option${selected ? ' active' : ''}`}
                  disabled={disabled}
                  aria-label={
                    action
                      ? `${action} for ${connection.name}`
                      : `Set ${connection.name} as default`
                  }
                  onClick={() => (action ? onEdit(connection, id) : onActivate(connection))}
                >
                  <span className="connection-detail">
                    {details}
                    {selected && <small>Selected default</small>}
                  </span>
                  <span className="default-model-action">{action || 'Set default'}</span>
                </button>
              );
            })}
          </div>
        ) : (
          <button
            type="button"
            className="add-connection"
            id="default-model-add"
            disabled={disabled || !settings.providers.length}
            onClick={() => onEdit(undefined, 'default-model-add')}
          >
            <Icon name="add" /> Add connection
          </button>
        ))}
      {load_error && (
        <p className="status connection-status error" role="alert">
          {load_error}
        </p>
      )}
      {!loading && (!settings || !!load_error) && (
        <button type="button" disabled={retry_disabled} onClick={onRetry}>
          Try again
        </button>
      )}
    </section>
  );
}
