import { useState } from 'react';
import { Icon } from '../Icon';

export function StoreAddress({
  store_url,
  busy,
  error,
  onSave,
  onCancel,
}: {
  store_url: string | null;
  busy: boolean;
  error: string;
  onSave: (value: string) => void;
  onCancel?: () => void;
}) {
  const [value, setValue] = useState(store_url ?? '');
  return (
    <section className="store-address">
      <div className="store-address-mark" aria-hidden="true">
        <Icon name="store" />
      </div>
      <h2>{store_url ? 'Store address' : 'Connect to a store'}</h2>
      <p>Browse applets, install them, and get updates from your store.</p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          onSave(value);
        }}
      >
        <label>
          Store URL
          <input
            type="url"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            placeholder="https://store.example.invalid"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            disabled={busy}
          />
        </label>
        <small>
          The address is saved on this device. Changing it keeps installed applets and their update
          sources.
        </small>
        {error && (
          <p className="status error" role="alert">
            {error}
          </p>
        )}
        <div className="store-form-actions">
          <button className="store-primary" disabled={busy || (!store_url && !value.trim())}>
            {busy ? 'Saving…' : 'Save store'}
          </button>
          {onCancel && (
            <button type="button" disabled={busy} onClick={onCancel}>
              Cancel
            </button>
          )}
        </div>
        {store_url && <small>Clear the address to disconnect this store.</small>}
      </form>
    </section>
  );
}
