import { useId } from 'react';
import type { CredentialStatus } from '../../lib/types';

interface ApiKeyFieldProps {
  credential_status?: CredentialStatus;
  mock_credentials: boolean;
  optional: boolean;
  value: string;
  removing: boolean;
  onChange: (value: string) => void;
  onRemove: () => void;
  onUndo: () => void;
}

export function ApiKeyField(props: ApiKeyFieldProps) {
  const help_id = useId();
  const has_saved_key =
    props.credential_status === 'present' || props.credential_status === 'unavailable';
  if (props.removing && has_saved_key) {
    return (
      <div className="credential-fields">
        <p className="status">The saved key will be removed when you save.</p>
        <button type="button" className="text-button" onClick={props.onUndo}>
          Undo removal
        </button>
      </div>
    );
  }

  const unavailable = props.credential_status === 'unavailable';
  const store = props.mock_credentials ? 'local' : 'secure';
  const saved_help = props.mock_credentials
    ? 'Key saved in an unencrypted test file. Enter a new test key to replace it.'
    : 'Key saved securely. Enter a new key only to replace it.';
  const help = unavailable
    ? `The ${store} credential store is unavailable. Leave this field blank to preserve the existing credential.`
    : has_saved_key
      ? saved_help
      : props.mock_credentials
        ? 'Use a test API key. Apell Local saves keys in unencrypted files.'
        : undefined;

  return (
    <div className="credential-fields">
      <label>
        {has_saved_key ? 'New API key' : props.optional ? 'API key (optional)' : 'API key'}
        <input
          type="password"
          required={!props.optional && !has_saved_key}
          maxLength={8192}
          value={props.value}
          placeholder={has_saved_key ? 'Leave blank to keep the saved key' : undefined}
          autoCapitalize="none"
          autoCorrect="off"
          autoComplete="off"
          aria-describedby={help ? help_id : undefined}
          spellCheck={false}
          onChange={(event) => props.onChange(event.target.value)}
        />
      </label>
      {help && (
        <p className={`status${unavailable ? ' error' : ''}`} id={help_id}>
          {help}
        </p>
      )}
      {has_saved_key && (
        <button type="button" className="text-button danger" onClick={props.onRemove}>
          Remove saved key
        </button>
      )}
    </div>
  );
}
