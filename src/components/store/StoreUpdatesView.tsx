import { storeChannelName } from '../../lib/storeState';
import type { StoreSource, StoreUpdateResults } from '../../lib/types';
import { AppletIcon } from '../AppletIcon';

export function StoreUpdatesView({
  report,
  loading,
  error,
  query,
  onRefresh,
  onSelect,
}: {
  report?: StoreUpdateResults;
  loading: boolean;
  error: string;
  query: string;
  onRefresh: () => void;
  onSelect: (source: StoreSource, trigger: HTMLButtonElement) => void;
}) {
  const search = query.trim().toLocaleLowerCase();
  const fresh = report?.fresh === true;
  const groups = (report?.sources ?? []).map((source) => ({
    source,
    unchecked: fresh ? source.installed_count - source.checked_count : 0,
    check_error: fresh ? source.error : undefined,
    matching: [
      ...source.updates.map((entry) => ({ ...entry, saved: !fresh })),
      ...source.saved_updates.map((entry) => ({ ...entry, saved: true })),
    ].filter(({ applet }) => applet.name.toLocaleLowerCase().includes(search)),
  }));
  const visible = groups.filter(
    ({ check_error, unchecked, matching }) => unchecked > 0 || check_error || matching.length > 0,
  );
  const no_installations = groups.every(({ source }) => source.installed_count === 0);
  const has_updates = groups.some(
    ({ source }) => source.updates.length || source.saved_updates.length,
  );
  return (
    <>
      {(loading || (!fresh && !error)) && (
        <p className="status" role="status">
          Checking installed applets for updates…
        </p>
      )}
      {error && (
        <div className="store-feedback" role="alert">
          <strong>Couldn’t check for updates</strong>
          <p>
            {has_updates ? 'Showing updates from previous checks.' : 'Update status is unknown.'}
          </p>
          <p>{error}</p>
          <button disabled={loading} onClick={onRefresh}>
            Try again
          </button>
        </div>
      )}
      {!loading && !error && fresh && !visible.length && (
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
      {visible.map(({ source, check_error, unchecked, matching }) => (
        <section className="store-update-group" key={source.store_url}>
          <h2>{source.store_url}</h2>
          {(unchecked > 0 || check_error) && (
            <div className="store-feedback" role="status">
              <strong>
                {unchecked === 1
                  ? 'Couldn’t check one applet'
                  : `Couldn’t check ${unchecked} applets`}
              </strong>
              {check_error && <p>{check_error}</p>}
            </div>
          )}
          {!!matching.length && (
            <ul className="store-list">
              {matching.map(({ applet, version, saved }) => {
                const installed_source = applet.source;
                if (installed_source?.kind !== 'store') return null;
                const version_id = `store-update-version-${applet.applet_id}`;
                return (
                  <li key={applet.applet_id}>
                    <button
                      className="store-card"
                      data-store-card={JSON.stringify([
                        installed_source.store_url,
                        applet.applet_id,
                        installed_source.channel,
                      ])}
                      onClick={(event) => onSelect(installed_source, event.currentTarget)}
                      aria-label={`${saved ? 'Check saved release' : 'Review update'} for ${applet.name}`}
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
                          {saved && (
                            <span className="store-badge store-badge-muted">Saved result</span>
                          )}
                          {applet.version} → {version}
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
