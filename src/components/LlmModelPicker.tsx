import { useEffect, useRef, useState } from 'react';
import type { LlmSelection } from '../../packages/applet-sdk/src/types';
import { publicSelection, selectableProvider, type ModelChoice } from '../lib/modelChoice';
import {
  createConnectionDraft,
  discardConnectionDraft,
  discoverDraftModels,
  getSettings,
  message,
} from '../lib/tauri';
import type { LlmConnection, LlmModels, LlmSettings } from '../lib/types';
import { Sheet } from './Sheet';
import { ModelPicker } from './providers/ModelPicker';
import { DraftSession } from './providers/state';

export function LlmModelPicker({ choice }: { choice: ModelChoice }) {
  const [settings, setSettings] = useState<LlmSettings>();
  const [connection_id, setConnectionId] = useState('');
  const [model_id, setModelId] = useState('');
  const [models, setModels] = useState<LlmModels>();
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState('');
  const lookup = useRef<DraftSession | undefined>(undefined);

  const stopLookup = () => {
    const previous = lookup.current;
    lookup.current = undefined;
    void previous?.discard().catch(() => {});
  };

  useEffect(() => {
    let closed = false;
    getSettings()
      .then((saved) => {
        if (closed) return;
        const connections = saved.connections.filter((entry) => entry.provider_id !== 'demo');
        const prior = connections.find(
          (entry) => entry.connection_id === choice.current?.connection_id,
        );
        const initial =
          prior ??
          connections.find((entry) => entry.connection_id === saved.active_connection_id) ??
          connections[0];
        setSettings(saved);
        setConnectionId(initial?.connection_id ?? '');
        setModelId(prior ? choice.current!.model_id : (initial?.model_id ?? ''));
      })
      .catch((error) => {
        if (!closed) setStatus(message(error));
      });
    return () => {
      closed = true;
      stopLookup();
    };
  }, [choice]);

  const connections = settings?.connections.filter((entry) => entry.provider_id !== 'demo') ?? [];
  const connection = connections.find((entry) => entry.connection_id === connection_id);
  const usable = settings && connection && selectableProvider(settings, connection);
  const selected =
    connection && settings ? publicSelection(settings, connection, model_id) : undefined;

  const loadModels = async (target: LlmConnection) => {
    stopLookup();
    const attempt = new DraftSession(createConnectionDraft, discardConnectionDraft);
    lookup.current = attempt;
    setLoading(true);
    setStatus('');
    try {
      const draft = await attempt.ensure({
        connection_id: target.connection_id,
        provider_id: target.provider_id,
        base_url: target.base_url,
      });
      if (lookup.current !== attempt) return;
      const result = await discoverDraftModels(draft.draft_id, { key_action: 'keep' });
      if (lookup.current !== attempt) return;
      setModels(result);
      if (!result.models.length) setStatus('No models were listed. Enter a model ID below.');
    } catch (error) {
      if (lookup.current === attempt) setStatus(message(error));
    } finally {
      if (lookup.current === attempt) {
        lookup.current = undefined;
        setLoading(false);
      }
      await attempt.discard().catch(() => {});
    }
  };

  const finish = (selection: LlmSelection | null) => {
    stopLookup();
    choice.finish(selection);
  };

  return (
    <Sheet open title="Choose a model" onClose={() => finish(null)}>
      <div className="applet-model-picker">
        {!settings ? (
          <p className="status" role="status">
            {status || 'Loading connections…'}
          </p>
        ) : !connections.length ? (
          <p>Add a provider in Settings to choose a model.</p>
        ) : (
          <>
            <label>
              Provider connection
              <select
                value={connection_id}
                onChange={(event) => {
                  const next = connections.find(
                    (entry) => entry.connection_id === event.target.value,
                  );
                  if (!next) return;
                  stopLookup();
                  setConnectionId(next.connection_id);
                  setModelId(
                    next.connection_id === choice.current?.connection_id
                      ? choice.current.model_id
                      : next.model_id,
                  );
                  setModels(undefined);
                  setStatus('');
                  setLoading(false);
                }}
              >
                {connections.map((entry) => (
                  <option key={entry.connection_id} value={entry.connection_id}>
                    {entry.name}
                  </option>
                ))}
              </select>
            </label>
            {usable && connection ? (
              <>
                <button
                  type="button"
                  disabled={loading}
                  onClick={() => void loadModels(connection)}
                >
                  {loading ? 'Loading models…' : models ? 'Refresh models' : 'Load models'}
                </button>
                <ModelPicker
                  key={connection_id}
                  model_id={model_id}
                  models={models}
                  disabled={false}
                  required
                  onChange={setModelId}
                />
                {status && (
                  <p className="status" role="status">
                    {status}
                  </p>
                )}
                <button
                  type="button"
                  className="primary"
                  disabled={!selected}
                  onClick={() => {
                    if (selected) finish(selected);
                  }}
                >
                  Use model
                </button>
              </>
            ) : (
              <p className="status">Update this connection in Settings to use it.</p>
            )}
          </>
        )}
        <button type="button" onClick={() => finish(null)}>
          {connections.length ? 'Cancel' : 'Close'}
        </button>
      </div>
    </Sheet>
  );
}
