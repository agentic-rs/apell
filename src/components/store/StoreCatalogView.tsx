import type { StoreCatalog } from '../../lib/types';
import { matchingStoreApplets, recommendedStoreRelease, storeSize } from '../../lib/storeState';
import { Icon } from '../Icon';
import { StoreIcon } from './StoreIcon';

export function StoreCatalogView({
  catalog,
  loading,
  error,
  query,
  updates_only,
  onQuery,
  onFilter,
  onRefresh,
  onAddress,
  onSelect,
}: {
  catalog?: StoreCatalog;
  loading: boolean;
  error: string;
  query: string;
  updates_only: boolean;
  onQuery: (value: string) => void;
  onFilter: (value: boolean) => void;
  onRefresh: () => void;
  onAddress: () => void;
  onSelect: (applet_id: string) => void;
}) {
  const applets = matchingStoreApplets(catalog?.applets ?? [], query, updates_only);
  const update_count =
    catalog?.applets.filter((item) => item.update_available && !item.source_conflict).length ?? 0;
  return (
    <section className="store-catalog" aria-label="Applet store" aria-busy={loading}>
      <div className="store-toolbar">
        <p>Find a new applet. Keep your favorites up to date.</p>
        <div className="store-toolbar-actions">
          <button
            className="icon-button"
            aria-label="Refresh store"
            title="Refresh store"
            disabled={loading}
            onClick={onRefresh}
          >
            <Icon name="reload" />
          </button>
          <button
            className="icon-button"
            aria-label="Store address"
            title="Store address"
            onClick={onAddress}
          >
            <Icon name="settings" />
          </button>
        </div>
      </div>
      <label className="store-search">
        <span>Search applets</span>
        <input
          type="search"
          value={query}
          onChange={(event) => onQuery(event.target.value)}
          placeholder="Name or description"
          autoCapitalize="none"
        />
      </label>
      <div className="store-filters" aria-label="Store filter">
        <button aria-pressed={!updates_only} onClick={() => onFilter(false)}>
          Discover
        </button>
        <button aria-pressed={updates_only} onClick={() => onFilter(true)}>
          Updates{update_count ? ` (${update_count})` : ''}
        </button>
      </div>
      {loading && (
        <p className="status" role="status">
          Checking the store…
        </p>
      )}
      {error && (
        <div className="store-feedback" role="alert">
          <strong>Couldn’t load the store</strong>
          <p>{error}</p>
          <button disabled={loading} onClick={onRefresh}>
            Try again
          </button>
        </div>
      )}
      {!loading && !error && !applets.length && (
        <div className="store-empty">
          <h2>
            {query.trim()
              ? 'No matching applets'
              : updates_only
                ? 'You’re up to date'
                : 'No applets yet'}
          </h2>
          <p>
            {query.trim()
              ? 'Try another name or a shorter search.'
              : updates_only
                ? 'Updates for applets installed from this store will appear here.'
                : 'Published applets will appear here when they’re ready.'}
          </p>
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
            return (
              <li key={applet.applet_id}>
                <button
                  className="store-card"
                  onClick={() => onSelect(applet.applet_id)}
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
                      {applet.update_available && !applet.source_conflict ? (
                        <span className="store-badge">Update available</span>
                      ) : applet.installed ? (
                        <span className="store-badge store-badge-muted">Installed</span>
                      ) : recommended.prerelease ? (
                        <span className="store-badge store-badge-preview">Preview</span>
                      ) : null}
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
    </section>
  );
}
