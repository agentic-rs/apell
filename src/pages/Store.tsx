import { useState } from 'react';
import type { AppletSession } from '../lib/types';
import { StoreAddress } from '../components/store/StoreAddress';
import { StoreCatalogView } from '../components/store/StoreCatalogView';
import { StoreDetailView } from '../components/store/StoreDetailView';
import { useStore } from '../components/store/useStore';
import './store.css';

export function Store({
  onLaunch,
  onBusyChange,
}: {
  onLaunch: (session: AppletSession) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const state = useStore(onLaunch, onBusyChange);
  const [query, setQuery] = useState('');
  const [updates_only, setUpdatesOnly] = useState(false);
  if (state.initial_loading)
    return (
      <p className="status" role="status">
        Opening the store…
      </p>
    );
  if (state.address_open || (state.config && !state.config.store_url))
    return (
      <StoreAddress
        key={state.config?.store_url}
        store_url={state.config?.store_url ?? null}
        busy={!!state.operation}
        error={state.action_error}
        onSave={state.saveAddress}
        onCancel={
          state.config?.store_url || state.config_error
            ? () => state.setAddressOpen(false)
            : undefined
        }
      />
    );
  if (!state.config)
    return (
      <section className="store-feedback" role="alert">
        <h2>Couldn’t open the store</h2>
        <p>{state.config_error}</p>
        <div className="store-form-actions">
          <button onClick={() => void state.loadConfig()}>Try again</button>
          <button onClick={() => state.setAddressOpen(true)}>Edit store address</button>
        </div>
      </section>
    );
  return (
    <div className="store-page">
      {state.status && (
        <p className="store-feedback store-success" role="status">
          {state.status}
        </p>
      )}
      {state.action_error && (
        <p className="store-feedback error" role="alert">
          {state.action_error}
        </p>
      )}
      {state.selected_id ? (
        state.detail ? (
          <StoreDetailView
            detail={state.detail}
            selected_version={state.selected_version}
            busy={!!state.operation}
            installing={state.operation === 'install'}
            progress={state.progress}
            onVersion={state.setSelectedVersion}
            onInstall={state.install}
            onOpen={state.openApplet}
            onBack={state.closeDetail}
          />
        ) : (
          <section className="store-detail-loading" aria-busy={state.detail_loading}>
            <button disabled={!!state.operation} onClick={state.closeDetail}>
              Back to applets
            </button>
            {state.detail_loading ? (
              <p className="status" role="status">
                Loading applet…
              </p>
            ) : (
              <div className="store-feedback" role="alert">
                <h2>Couldn’t load this applet</h2>
                <p>{state.detail_error}</p>
                <button onClick={state.retryDetail}>Try again</button>
              </div>
            )}
          </section>
        )
      ) : (
        <StoreCatalogView
          catalog={state.catalog}
          loading={state.catalog_loading}
          error={state.catalog_error}
          query={query}
          updates_only={updates_only}
          onQuery={setQuery}
          onFilter={setUpdatesOnly}
          onRefresh={state.refreshCatalog}
          onAddress={() => state.setAddressOpen(true)}
          onSelect={state.openDetail}
        />
      )}
    </div>
  );
}
