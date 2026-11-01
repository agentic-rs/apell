import type { CredentialStatus } from '../../lib/types';

interface ApiKeyFieldProps {
  credential_status?: CredentialStatus;
  optional: boolean;
  value: string;
  removing: boolean;
  onChange: (value: string) => void;
  onRemove: () => void;
  onUndo: () => void;
}

export function ApiKeyField(props: ApiKeyFieldProps) {
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
          spellCheck={false}
          onChange={(event) => props.onChange(event.target.value)}
        />
      </label>
      {props.credential_status === 'unavailable' ? (
        <p className="status error">
          The secure credential store is unavailable. Leave this field blank to preserve the
          existing credential.
        </p>
      ) : props.credential_status === 'present' ? (
        <p className="status">Key saved securely. Enter a new key only to replace it.</p>
      ) : null}
      {has_saved_key && (
        <button type="button" className="text-button danger" onClick={props.onRemove}>
          Remove saved key
        </button>
      )}
    </div>
  );
}
