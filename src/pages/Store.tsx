import { useLayoutEffect, useRef, useState } from 'react';
import type { AppletSession } from '../lib/types';
import { storeChannelName } from '../lib/storeState';
import { useBackAction } from '../lib/backNavigation';
import { AppletIcon } from '../components/AppletIcon';
import { Sheet } from '../components/Sheet';
import { StoreAddress } from '../components/store/StoreAddress';
import { StoreCatalogView } from '../components/store/StoreCatalogView';
import { StoreDetailView, StoreOpenButton } from '../components/store/StoreDetailView';
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
  const opening = state.operation === 'open' || state.operation === 'restart';
  const installed_source = state.installed?.source;
  const reviewing_download = state.reviewing_download;
  const page = state.address_open
    ? 'address'
    : state.selected_id
      ? 'detail'
      : updates_only
        ? 'updates'
        : 'discover';
  const previous_page = useRef(page);
  const previous_origin = useRef(state.config?.store_url);
  const catalog_scroll = useRef({ discover: 0, updates: 0 });
  useLayoutEffect(() => {
    if (previous_origin.current !== state.config?.store_url) {
      previous_origin.current = state.config?.store_url;
      catalog_scroll.current.discover = 0;
    }
    if (previous_page.current === page) return;
    previous_page.current = page;
    window.scrollTo(
      0,
      page === 'discover' || page === 'updates' ? catalog_scroll.current[page] : 0,
    );
  }, [page, state.config?.store_url]);
  const rememberCatalogScroll = () => {
    if (page === 'discover' || page === 'updates') catalog_scroll.current[page] = window.scrollY;
  };
  useBackAction(25, state.address_open || !!state.selected_id, () => {
    if (state.operation) return;
    if (state.address_open) state.setAddressOpen(false);
    else state.closeDetail();
  });
  if (state.initial_loading)
    return (
      <p className="status" role="status">
        Opening the store…
      </p>
    );
  if (state.address_open)
    return (
      <StoreAddress
        key={state.config?.store_url}
        store_url={state.config?.store_url ?? null}
        busy={!!state.operation}
        error={state.action_error}
        onSave={state.saveAddress}
        onCancel={() => state.setAddressOpen(false)}
      />
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
        state.detail && state.review ? (
          <StoreDetailView
            detail={state.detail}
            installed_source={state.installed_source}
            reviewing_download={reviewing_download}
            restart_required={state.restart_required}
            review={state.review}
            fresh={state.detail_fresh}
            refreshing={state.detail_loading}
            busy={!!state.operation}
            installing={state.operation === 'install'}
            opening={opening}
            progress={state.progress}
            onChannel={state.setChannel}
            onVersion={state.setSelectedVersion}
            onInstall={state.install}
            onOpen={state.openApplet}
            onRefresh={state.retryDetail}
            onBack={state.closeDetail}
          />
        ) : (
          <section className="store-detail-loading" aria-busy={state.detail_loading}>
            <button className="store-back" disabled={!!state.operation} onClick={state.closeDetail}>
              {reviewing_download
                ? 'Back to Store'
                : state.installed_source
                  ? 'Back to updates'
                  : 'Back to applets'}
            </button>
            {state.installed && (
              <div className="store-download-panel">
                <div className="store-detail-heading">
                  <span className="store-icon store-icon-large" aria-hidden="true">
                    <AppletIcon applet={state.installed} />
                  </span>
                  <div>
                    <h2>{state.installed.name}</h2>
                    <p>
                      Installed · {state.installed.version}
                      {installed_source?.kind === 'store' &&
                        ` · ${storeChannelName(installed_source.channel)}`}
                    </p>
                  </div>
                </div>
                <div className="store-install-actions">
                  <StoreOpenButton
                    restart_required={state.restart_required}
                    busy={!!state.operation}
                    opening={opening}
                    onOpen={state.openApplet}
                  />
                </div>
              </div>
            )}
            {state.detail_loading ? (
              <p className="status" role="status">
                Loading applet…
              </p>
            ) : (
              <div className="store-feedback" role="alert">
                <h2>
                  {reviewing_download
                    ? 'Couldn’t review this download'
                    : state.installed
                      ? 'Couldn’t load Store releases'
                      : 'Couldn’t load this applet'}
                </h2>
                {reviewing_download && (
                  <p>
                    {reviewing_download.name} · {reviewing_download.version} ·{' '}
                    {storeChannelName(reviewing_download.channel)}
                  </p>
                )}
                <p>{state.detail_error}</p>
                <button onClick={state.retryDetail}>Try again</button>
              </div>
            )}
          </section>
        )
      ) : (
        <StoreCatalogView
          catalog={state.catalog}
          store_url={state.config?.store_url}
          updates={state.updates}
          pending_download={state.pending_download}
          loading={updates_only ? state.updates_loading : state.catalog_loading}
          error={updates_only ? state.updates_error : state.catalog_error || state.config_error}
          query={updates_only ? state.updates_query : query}
          updates_only={updates_only}
          onQuery={updates_only ? state.setUpdatesQuery : (query) => state.updateBrowse({ query })}
          onFilter={(next) => {
            if (next === updates_only) return;
            rememberCatalogScroll();
            state.updateBrowse({ updates_only: next });
          }}
          onRefresh={updates_only ? state.refreshUpdates : state.refreshCatalog}
          onAddress={() => {
            rememberCatalogScroll();
            state.setAddressOpen(true);
          }}
          onSelect={(applet_id) => {
            rememberCatalogScroll();
            state.openDetail(applet_id);
          }}
          onSelectUpdate={(source) => {
            rememberCatalogScroll();
            state.openUpdate(source);
          }}
          onReviewDownload={() => {
            rememberCatalogScroll();
            state.reviewDownload();
          }}
        />
      )}
      {reviewing_download && (
        <DiscardDownload
          name={reviewing_download.name}
          version={reviewing_download.version}
          disabled={!!state.operation}
          onDiscard={state.discardDownload}
        />
      )}
    </div>
  );
}

function DiscardDownload({
  name,
  version,
  disabled,
  onDiscard,
}: {
  name: string;
  version: string;
  disabled: boolean;
  onDiscard: () => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="store-form-actions store-discard-actions">
      <button disabled={disabled} onClick={() => setOpen(true)}>
        Discard download
      </button>
      <Sheet title="Discard paused download?" open={open} onClose={() => setOpen(false)}>
        <p>
          Apell will clear the saved download progress for {name} {version}.
        </p>
        <div className="store-form-actions">
          <button onClick={() => setOpen(false)}>Keep download</button>
          <button
            className="danger"
            onClick={() => {
              setOpen(false);
              onDiscard();
            }}
          >
            Discard download
          </button>
        </div>
      </Sheet>
    </div>
  );
}
