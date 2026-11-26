import { connectionReadinessLabel, type ConnectionReadiness } from '../../lib/modelChoice';
import type { LlmConnection } from '../../lib/types';

export function SavedConnectionDetails({
  connection,
  readiness,
}: {
  connection: LlmConnection;
  readiness: ConnectionReadiness;
}) {
  const provider = readiness.provider?.name ?? connection.provider_id;
  const problem = connectionReadinessLabel(readiness);
  return (
    <>
      <strong>{connection.name}</strong>
      {connection.model_id.trim() && <span>{connection.model_id}</span>}
      <small>
        {provider}
        {problem && ` · ${problem}`}
      </small>
    </>
  );
}
