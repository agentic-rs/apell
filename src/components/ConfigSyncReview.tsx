import { useId } from 'react';
import { compareConfigSync } from '../lib/configSyncReview';
import type { ConfigSyncConflict, ConfigSyncConnection, LlmProvider } from '../lib/types';

interface Props {
  conflict: ConfigSyncConflict;
  providers: LlmProvider[];
  busy: boolean;
  disabled: boolean;
  stale: boolean;
  error: string;
  onRefresh: () => void;
  onKeepLocal: () => void;
  onUseCloud: () => void;
}

function ConnectionDescription({
  connection,
  provider_names,
  ambiguous_ids,
}: {
  connection?: ConfigSyncConnection;
  provider_names: Map<string, string>;
  ambiguous_ids: Set<string>;
}) {
  return connection ? (
    <>
      <strong>{connection.name}</strong>
      <small>
        {provider_names.get(connection.provider_id) ?? connection.provider_id} ·{' '}
        {connection.model_id}
      </small>
      {ambiguous_ids.has(connection.connection_id) && (
        <small>Connection ID {connection.connection_id}</small>
      )}
    </>
  ) : (
    'None'
  );
}

function CloudApplyDescription({
  blocker,
}: {
  blocker: ConfigSyncConflict['cloud_apply_blocker'];
}) {
  if (blocker?.kind === 'destination_conflict') {
    return (
      <>
        “{blocker.name}” conflicts with a cloud connection. In Model connections, change its
        provider or endpoint, or remove it. Then refresh this review.
      </>
    );
  }
  if (blocker?.kind === 'connection_limit') {
    const { extra_connections } = blocker;
    return (
      <>
        Free {extra_connections} {extra_connections === 1 ? 'slot' : 'slots'} in Model connections
        by removing connections excluded from sync, such as local models or custom endpoints. Then
        refresh this review.
      </>
    );
  }
  return 'Apply your account’s model settings to this device.';
}

export function ConfigSyncReview({
  conflict,
  providers,
  busy,
  disabled,
  stale,
  error,
  onRefresh,
  onKeepLocal,
  onUseCloud,
}: Props) {
  const cloud_description_id = useId();
  const blocker = conflict.cloud_apply_blocker;
  const { preferred, changes, removed_count, ambiguous_ids } = compareConfigSync(
    conflict.local,
    conflict.cloud,
  );
  const provider_names = new Map(
    providers.map((provider) => [provider.provider_id, provider.name]),
  );
  const renderConnection = (connection?: ConfigSyncConnection) => (
    <ConnectionDescription {...{ connection, provider_names, ambiguous_ids }} />
  );
  return (
    <div className="transfer-form sync-review" aria-busy={busy}>
      {preferred && (
        <section className="sync-review-section">
          <h3>Preferred synced connection</h3>
          <dl className="sync-review-pair">
            <div>
              <dt>This device</dt>
              <dd>{renderConnection(preferred.local)}</dd>
            </div>
            <div>
              <dt>Cloud</dt>
              <dd>{renderConnection(preferred.cloud)}</dd>
            </div>
          </dl>
        </section>
      )}
      {changes.length > 0 && (
        <section className="sync-review-section">
          <h3>Connection changes</h3>
          <ul className="sync-review-changes">
            {changes.map(({ local, cloud }) => (
              <li key={local?.connection_id ?? cloud?.connection_id}>
                <dl className="sync-review-pair">
                  {local && (
                    <div>
                      <dt>{cloud ? 'This device' : 'Only on this device'}</dt>
                      <dd>{renderConnection(local)}</dd>
                    </div>
                  )}
                  {cloud && (
                    <div>
                      <dt>{local ? 'Cloud' : 'Only in cloud'}</dt>
                      <dd>{renderConnection(cloud)}</dd>
                    </div>
                  )}
                </dl>
              </li>
            ))}
          </ul>
        </section>
      )}
      {error && (
        <p className="status error" role="alert">
          {error}
        </p>
      )}
      <div className="sync-review-options">
        <div>
          <button type="button" disabled={disabled || busy || stale} onClick={onKeepLocal}>
            Sync this device to cloud
          </button>
          <p>Replace your account’s saved model settings with this device’s copy.</p>
        </div>
        <div>
          <button
            type="button"
            disabled={disabled || busy || stale || !!blocker}
            aria-describedby={cloud_description_id}
            onClick={onUseCloud}
          >
            Use cloud on this device
          </button>
          <p id={cloud_description_id}>
            <CloudApplyDescription blocker={blocker} />
          </p>
          {!blocker && removed_count > 0 && (
            <p>
              Connections marked Only on this device and their saved credentials will be removed.
            </p>
          )}
        </div>
      </div>
      <button type="button" disabled={disabled || busy} onClick={onRefresh}>
        Refresh review
      </button>
      {busy && <p role="status">Checking model settings…</p>}
    </div>
  );
}
