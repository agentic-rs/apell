import { useState } from 'react';
import type { LlmModels } from '../../lib/types';
import { matchingModels } from './state';

export function ModelPicker({
  model_id,
  models,
  disabled,
  onChange,
}: {
  model_id: string;
  models?: LlmModels;
  disabled: boolean;
  onChange: (model_id: string) => void;
}) {
  const [query, setQuery] = useState('');
  const matches = matchingModels(models?.models ?? [], query);
  return (
    <div className="model-picker">
      <label>
        Model ID
        <input
          value={model_id}
          onChange={(event) => onChange(event.target.value)}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          disabled={disabled}
          placeholder="Choose a model or enter its ID"
        />
      </label>
      {models && models.models.length > 0 && (
        <details className="model-browser" open>
          <summary>
            {models.models.length} {models.source === 'catalogue' ? 'catalogue' : 'available'}{' '}
            models
          </summary>
          <label>
            <span className="visually-hidden">Find a model</span>
            <input
              type="search"
              placeholder="Find a model"
              value={query}
              disabled={disabled}
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
          <div className="model-options" aria-label="Available models">
            {matches.slice(0, 40).map((model) => (
              <button
                type="button"
                key={model.model_id}
                disabled={disabled}
                aria-pressed={model_id === model.model_id}
                onClick={() => onChange(model.model_id)}
              >
                <strong>{model.name || model.model_id}</strong>
                {model.name && model.name !== model.model_id && <small>{model.model_id}</small>}
              </button>
            ))}
            {!matches.length && (
              <p className="status">No matching models. You can enter an ID above.</p>
            )}
          </div>
          {matches.length > 40 && (
            <p className="status">Showing 40 matches. Refine your search to see more.</p>
          )}
          {models.source === 'catalogue' && (
            <p className="status">Catalogue entries may not be available to this account.</p>
          )}
        </details>
      )}
    </div>
  );
}
