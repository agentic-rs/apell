import type { ConfigSyncConnection, ConfigSyncSummary } from './types';

export interface ConfigSyncChange {
  local?: ConfigSyncConnection;
  cloud?: ConfigSyncConnection;
}

interface ConfigSyncComparison {
  preferred?: ConfigSyncChange;
  changes: ConfigSyncChange[];
  removed_count: number;
  ambiguous_ids: Set<string>;
}

function sameConnection(left?: ConfigSyncConnection, right?: ConfigSyncConnection): boolean {
  return (
    left?.connection_id === right?.connection_id &&
    left?.name === right?.name &&
    left?.provider_id === right?.provider_id &&
    left?.model_id === right?.model_id
  );
}

function ambiguousConnections(connections: ConfigSyncConnection[]): Set<string> {
  const first_id_by_label = new Map<string, string>();
  const ambiguous_ids = new Set<string>();
  for (const { connection_id, name, provider_id, model_id } of connections) {
    const label_key = JSON.stringify([name, provider_id, model_id]);
    const first_id = first_id_by_label.get(label_key);
    if (first_id === undefined) {
      first_id_by_label.set(label_key, connection_id);
    } else if (first_id !== connection_id) {
      ambiguous_ids.add(first_id);
      ambiguous_ids.add(connection_id);
    }
  }
  return ambiguous_ids;
}

export function compareConfigSync(
  local: ConfigSyncSummary,
  cloud: ConfigSyncSummary,
): ConfigSyncComparison {
  const local_by_id = new Map(
    local.connections.map((connection) => [connection.connection_id, connection]),
  );
  const cloud_by_id = new Map(
    cloud.connections.map((connection) => [connection.connection_id, connection]),
  );
  const changes: ConfigSyncChange[] = [];
  let removed_count = 0;

  for (const connection of local.connections) {
    const counterpart = cloud_by_id.get(connection.connection_id);
    if (sameConnection(connection, counterpart)) continue;
    if (!counterpart) removed_count += 1;
    changes.push({ local: connection, cloud: counterpart });
  }
  for (const connection of cloud.connections) {
    if (!local_by_id.has(connection.connection_id)) changes.push({ cloud: connection });
  }

  const local_preferred =
    local.preferred_connection_id === null
      ? undefined
      : local_by_id.get(local.preferred_connection_id);
  const cloud_preferred =
    cloud.preferred_connection_id === null
      ? undefined
      : cloud_by_id.get(cloud.preferred_connection_id);
  return {
    preferred: sameConnection(local_preferred, cloud_preferred)
      ? undefined
      : { local: local_preferred, cloud: cloud_preferred },
    changes,
    removed_count,
    ambiguous_ids: ambiguousConnections([...local.connections, ...cloud.connections]),
  };
}
