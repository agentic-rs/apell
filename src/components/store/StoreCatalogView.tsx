import { useRef, type Ref } from 'react';
import type { StoreCatalog, StoreSource, StoreUpdateResults } from '../../lib/types';
import {
  installedStoreChannel,
  matchingStoreApplets,
  recommendedStoreRelease,
  storeChannelName,
  storeSize,
} from '../../lib/storeState';
import { Icon } from '../Icon';
import { StoreIcon } from './StoreIcon';
import { StoreUpdatesView } from './StoreUpdatesView';

export function StoreCatalogView({
  catalog,
  store_url,
  updates,
  retrying_store,
  updates_notice,
  download_count,
  loading,
  error,
  query,
  updates_only,
  catalog_ref,
  onQuery,
  onFilter,
  onRefresh,
  onRetryUpdateSource,
  onAddress,
  onSelect,
  onSelectUpdate,
  onOpenDownloads,
}: {
  catalog?: StoreCatalog;
  store_url?: string | null;
  updates?: StoreUpdateResults;
  retrying_store?: string;
  updates_notice?: string;
  download_count: number;
  loading: boolean;
  error: string;
  query: string;
  updates_only: boolean;
  catalog_ref?: Ref<HTMLElement>;
  onQuery: (value: string) => void;
  onFilter: (value: boolean) => void;
  onRefresh: () => void;
  onRetryUpdateSource: (store_url: string) => void;
  onAddress: (trigger: HTMLButtonElement) => void;
  onSelect: (applet_id: string, trigger: HTMLButtonElement) => void;
  onSelectUpdate: (source: StoreSource, trigger: HTMLButtonElement) => void;
  onOpenDownloads: () => void;
}) {
  const updates_tab = useRef<HTMLButtonElement>(null);
  const update_count = updates?.fresh
    ? updates.sources.reduce((count, source) => count + source.updates.length, 0)
    : 0;
  const refresh_label = updates_only ? 'Refresh updates' : 'Refresh store';
  return (
    <section
      ref={catalog_ref}
      className="store-catalog"
      aria-label="Applet store"
      aria-busy={loading}
    >
      <div className="store-toolbar">
        <p>
          {updates_only
            ? 'Check for updates to applets installed from any Store.'
            : 'Find a new applet. Keep your favorites up to date.'}
        </p>
        <div className="store-toolbar-actions">
          {(updates_only || store_url || error) && (
            <button
              className="icon-button"
              aria-label={refresh_label}
              title={refresh_label}
              disabled={loading || (updates_only && !!retrying_store)}
              onClick={onRefresh}
            >
              <Icon name="reload" />
            </button>
          )}
          <button
            id="store-address-settings"
            className="icon-button"
            aria-label="Store address"
            title="Store address"
            onClick={(event) => onAddress(event.currentTarget)}
          >
            <Icon name="settings" />
          </button>
        </div>
      </div>
      {(store_url || updates_only) && (
        <label className="store-search">
          <span>{updates_only ? 'Search updates' : 'Search applets'}</span>
          <input
            type="search"
            value={query}
            maxLength={200}
            onChange={(event) => onQuery(event.target.value)}
            placeholder={updates_only ? 'Applet name' : 'Name or description'}
            autoCapitalize="none"
          />
        </label>
      )}
      <div className="store-filters" role="group" aria-label="Store views">
        <button aria-pressed={!updates_only} onClick={() => onFilter(false)}>
          Discover
        </button>
        <button ref={updates_tab} aria-pressed={updates_only} onClick={() => onFilter(true)}>
          Updates{update_count ? ` (${update_count})` : ''}
        </button>
        <button id="store-open-downloads" type="button" onClick={onOpenDownloads}>
          Downloads{download_count ? ` (${download_count})` : ''}
        </button>
      </div>
      {updates_only ? (
        <StoreUpdatesView
          report={updates}
          retrying_store={retrying_store}
          notice={updates_notice}
          query={query}
          loading={loading}
          error={error}
          onRefresh={onRefresh}
          onRetrySource={onRetryUpdateSource}
          updates_tab={updates_tab}
          onSelect={onSelectUpdate}
        />
      ) : (
        <StoreDiscover
          catalog={catalog}
          loading={loading}
          error={error}
          query={query}
          store_url={store_url}
          onRefresh={onRefresh}
          onAddress={onAddress}
          onSelect={onSelect}
        />
      )}
    </section>
  );
}

function StoreDiscover({
  catalog,
  loading,
  error,
  query,
  store_url,
  onRefresh,
  onAddress,
  onSelect,
}: {
  catalog?: StoreCatalog;
  loading: boolean;
  error: string;
  query: string;
  store_url?: string | null;
  onRefresh: () => void;
  onAddress: (trigger: HTMLButtonElement) => void;
  onSelect: (applet_id: string, trigger: HTMLButtonElement) => void;
}) {
  const applets = matchingStoreApplets(catalog?.applets ?? [], query, false);
  return (
    <>
      {loading && (
        <p className="status" role="status">
          {catalog ? 'Showing saved listings while checking the Store…' : 'Checking the Store…'}
        </p>
      )}
      {error && (
        <div className="store-feedback" role="alert">
          <strong>{catalog ? 'Couldn’t refresh the store' : 'Couldn’t load the store'}</strong>
          <p>{error}</p>
          {catalog && <p>Showing saved listings.</p>}
          <button disabled={loading} onClick={onRefresh}>
            Try again
          </button>
        </div>
      )}
      {!loading && !error && !applets.length && (
        <div className="store-empty">
          {store_url ? (
            <>
              <h2>{query.trim() ? 'No matching applets' : 'No applets yet'}</h2>
              <p>
                {query.trim()
                  ? 'Try another name or a shorter search.'
                  : 'Published applets will appear here.'}
              </p>
            </>
          ) : (
            <>
              <h2>Connect to a store</h2>
              <button id="store-connect" onClick={(event) => onAddress(event.currentTarget)}>
                Set store address
              </button>
            </>
          )}
        </div>
      )}
      {!!applets.length && catalog && (
        <ul className="store-list">
          {applets.map((applet) => {
            const recommended = recommendedStoreRelease({
              store_url: catalog.store_url,
              applet,
              installed: applet.installed,
            });
            const installed_channel = installedStoreChannel(
              catalog.store_url,
              applet.applet_id,
              applet.installed,
            );
            const update_available = applet.update_available && !applet.source_conflict;
            const channel_name = installed_channel && storeChannelName(installed_channel);
            return (
              <li key={applet.applet_id}>
                <button
                  className="store-card"
                  data-store-card={JSON.stringify([catalog.store_url, applet.applet_id])}
                  onClick={(event) => onSelect(applet.applet_id, event.currentTarget)}
                  aria-label={`View ${applet.name}`}
                >
                  <StoreIcon
                    store_url={catalog.store_url}
                    icon_url={applet.icon_url}
                    name={applet.name}
                  />
                  <span className="store-card-copy">
                    <strong>{applet.name}</strong>
                    <span className="store-card-description">{applet.description}</span>
                    <span className="store-card-meta">
                      {applet.installation_unavailable ? (
                        <span className="store-badge store-badge-muted">
                          Installation unavailable
                        </span>
                      ) : update_available ? (
                        <span className="store-badge">
                          {channel_name ? `Update available · ${channel_name}` : 'Update available'}
                        </span>
                      ) : applet.installed ? (
                        <span className="store-badge store-badge-muted">
                          {applet.source_conflict
                            ? 'Installed from another source'
                            : channel_name
                              ? `Installed · ${channel_name}`
                              : 'Installed'}
                        </span>
                      ) : null}
                      {!installed_channel && recommended.prerelease && (
                        <span className="store-badge store-badge-preview">Nightly</span>
                      )}
                      <span>{storeSize(recommended.size_bytes)}</span>
                    </span>
                  </span>
                  <span className="store-card-arrow" aria-hidden="true">
                    ›
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {catalog && <p className="store-origin">{catalog.store_url}</p>}
    </>
  );
}
