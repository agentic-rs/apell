import { useLayoutEffect, useRef } from 'react';
import type { AppletSession } from '../lib/types';
import { useBackAction } from '../lib/backNavigation';
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
  const { query, updates_only } = state.browse;
  const page = state.address_open ? 'address' : state.selected_id ? 'detail' : 'catalog';
  const previous_page = useRef(page);
  const previous_origin = useRef(state.config?.store_url);
  const catalog_scroll = useRef(0);
  useLayoutEffect(() => {
    if (previous_origin.current !== state.config?.store_url) {
      previous_origin.current = state.config?.store_url;
      catalog_scroll.current = 0;
    }
    if (previous_page.current === page) return;
    previous_page.current = page;
    window.scrollTo(0, page === 'catalog' ? catalog_scroll.current : 0);
  }, [page, state.config?.store_url]);
  const rememberCatalogScroll = () => {
    if (page === 'catalog') catalog_scroll.current = window.scrollY;
  };
  const can_leave_address = state.address_open && !!(state.config?.store_url || state.config_error);
  useBackAction(25, can_leave_address || !!state.selected_id, () => {
    if (state.operation) return;
    if (can_leave_address) state.setAddressOpen(false);
    else state.closeDetail();
  });
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
      {!state.detail && state.restart_after_install && (
        <div className="store-form-actions">
          <button className="store-primary" disabled={!!state.operation} onClick={state.openApplet}>
            {state.operation === 'restart' ? 'Restarting…' : 'Restart applet'}
          </button>
        </div>
      )}
      {state.action_error && (
        <p className="store-feedback error" role="alert">
          {state.action_error}
        </p>
      )}
      {state.detail && state.detail_error && (
        <div className="store-feedback" role="alert">
          <strong>Couldn’t refresh this applet</strong>
          <p>{state.detail_error}</p>
          <button disabled={!!state.operation} onClick={state.retryDetail}>
            Try again
          </button>
        </div>
      )}
      {state.selected_id ? (
        state.detail ? (
          <StoreDetailView
            detail={state.detail}
            selected_version={state.selected_version}
            fresh={state.detail_fresh}
            refreshing={state.detail_loading}
            busy={!!state.operation}
            installing={state.operation === 'install'}
            restarting={state.operation === 'restart'}
            progress={state.progress}
            onVersion={state.setSelectedVersion}
            onInstall={state.install}
            onOpen={state.openApplet}
            onRefresh={state.retryDetail}
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
          onQuery={(query) => state.updateBrowse({ query })}
          onFilter={(updates_only) => state.updateBrowse({ updates_only })}
          onRefresh={state.refreshCatalog}
          onAddress={() => {
            rememberCatalogScroll();
            state.setAddressOpen(true);
          }}
          onSelect={(applet_id) => {
            rememberCatalogScroll();
            state.openDetail(applet_id);
          }}
        />
      )}
    </div>
  );
}
