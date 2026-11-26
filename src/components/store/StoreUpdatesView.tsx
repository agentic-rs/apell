import { storeChannelName } from '../../lib/storeState';
import type { StoreSource, StoreUpdatesReport } from '../../lib/types';
import { AppletIcon } from '../AppletIcon';

export function StoreUpdatesView({
  report,
  loading,
  error,
  query,
  onRefresh,
  onSelect,
}: {
  report?: StoreUpdatesReport;
  loading: boolean;
  error: string;
  query: string;
  onRefresh: () => void;
  onSelect: (source: StoreSource) => void;
}) {
  const search = query.trim().toLocaleLowerCase();
  const groups = (report?.sources ?? []).map((source) => ({
    source,
    unchecked: source.installed_count - source.checked_count,
    matching: source.updates.filter(({ applet }) =>
      applet.name.toLocaleLowerCase().includes(search),
    ),
  }));
  const visible = groups.filter(
    ({ source, unchecked, matching }) => unchecked > 0 || source.error || matching.length > 0,
  );
  const no_installations = groups.every(({ source }) => source.installed_count === 0);
  const has_updates = groups.some(({ source }) => source.updates.length > 0);
  return (
    <>
      {(loading || (!report && !error)) && (
        <p className="status" role="status">
          Checking installed applets for updates…
        </p>
      )}
      {error && (
        <div className="store-feedback" role="alert">
          <strong>Couldn’t check for updates</strong>
          <p>
            {has_updates
              ? 'Shown updates are from the previous check.'
              : 'Update status is unknown.'}
          </p>
          <p>{error}</p>
          <button disabled={loading} onClick={onRefresh}>
            Try again
          </button>
        </div>
      )}
      {!loading && !error && report && !visible.length && (
        <div className="store-empty">
          <h2>
            {no_installations
              ? 'No Store applets installed'
              : has_updates
                ? 'No matching updates'
                : 'No updates available'}
          </h2>
          <p>
            {no_installations
              ? 'Applets installed from a Store will be checked here.'
              : has_updates
                ? 'Try another name or clear your search.'
                : 'Apell checked each installed Store applet on its current channel.'}
          </p>
        </div>
      )}
      {visible.map(({ source, unchecked, matching }) => (
        <section className="store-update-group" key={source.store_url}>
          <h2>{source.store_url}</h2>
          {(unchecked > 0 || source.error) && (
            <div className="store-feedback" role="status">
              <strong>
                {unchecked === 1
                  ? 'Couldn’t check one applet'
                  : `Couldn’t check ${unchecked} applets`}
              </strong>
              {source.error && <p>{source.error}</p>}
            </div>
          )}
          {!!matching.length && (
            <ul className="store-list">
              {matching.map(({ applet, release }) => {
                const installed_source = applet.source;
                if (installed_source?.kind !== 'store') return null;
                const version_id = `store-update-version-${applet.applet_id}`;
                return (
                  <li key={applet.applet_id}>
                    <button
                      className="store-card"
                      onClick={() => onSelect(installed_source)}
                      aria-label={`Review update for ${applet.name}`}
                      aria-describedby={version_id}
                    >
                      <span className="store-icon" aria-hidden="true">
                        <AppletIcon applet={applet} />
                      </span>
                      <span className="store-card-copy">
                        <strong>{applet.name}</strong>
                        <span className="store-card-meta" id={version_id}>
                          <span className="store-badge">
                            {storeChannelName(installed_source.channel)}
                          </span>
                          {applet.version} → {release.version}
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
        </section>
      ))}
    </>
  );
}
