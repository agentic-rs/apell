import { useLayoutEffect, useRef, type RefObject } from 'react';
import { storeChannelName } from '../../lib/storeState';
import type { StoreSource, StoreUpdateResults } from '../../lib/types';
import { AppletIcon } from '../AppletIcon';

export function StoreUpdatesView({
  report,
  retrying_store,
  notice,
  loading,
  error,
  query,
  onRefresh,
  onRetrySource,
  updates_tab,
  onSelect,
}: {
  report?: StoreUpdateResults;
  retrying_store?: string;
  notice?: string;
  loading: boolean;
  error: string;
  query: string;
  onRefresh: () => void;
  onRetrySource: (store_url: string) => void;
  updates_tab: RefObject<HTMLButtonElement | null>;
  onSelect: (source: StoreSource, trigger: HTMLButtonElement) => void;
}) {
  const retry_focus = useRef<{ button: HTMLButtonElement; heading?: HTMLElement | null }>(
    undefined,
  );
  useLayoutEffect(() => {
    const previous = retry_focus.current;
    if (retrying_store || !previous) return;
    retry_focus.current = undefined;
    if (document.activeElement !== document.body && document.activeElement !== previous.button)
      return;
    const { button, heading } = previous;
    const target =
      button.isConnected && !button.disabled
        ? button
        : heading?.isConnected
          ? heading
          : updates_tab.current;
    target?.focus();
  }, [retrying_store, report, loading, updates_tab]);
  const search = query.trim().toLocaleLowerCase();
  const fresh = report?.fresh === true;
  const groups = (report?.sources ?? []).map((source) => ({
    source,
    unchecked: fresh ? source.installed_count - source.checked_count : 0,
    check_error: fresh ? source.error : undefined,
    rows: [
      ...source.updates.map((entry) => ({ ...entry, saved: !fresh })),
      ...source.saved_updates.map((entry) => ({ ...entry, saved: true })),
      ...(fresh ? source.unresolved_applets : []),
    ].filter(({ applet }) => applet.name.toLocaleLowerCase().includes(search)),
  }));
  const visible = groups.filter(
    ({ check_error, unchecked, rows }) => unchecked > 0 || check_error || rows.length > 0,
  );
  const no_installations = groups.every(({ source }) => source.installed_count === 0);
  const has_updates = groups.some(
    ({ source }) => source.updates.length || source.saved_updates.length,
  );
  const has_unresolved = fresh && groups.some(({ source }) => source.unresolved_applets.length);
  const no_matches =
    search !== '' && (has_updates || has_unresolved) && groups.every(({ rows }) => !rows.length);
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
      {notice && (
        <p className="store-feedback store-success" role="status">
          {notice}
        </p>
      )}
      {no_matches && (
        <div className="store-empty">
          <h2>No matching applets</h2>
          <p>Try another name or clear your search.</p>
        </div>
      )}
      {!loading && !error && fresh && !visible.length && !no_matches && (
        <div className="store-empty">
          <h2>{no_installations ? 'No Store applets installed' : 'No updates available'}</h2>
          <p>
            {no_installations
              ? 'Applets installed from a Store will be checked here.'
              : 'Apell checked each installed Store applet on its current channel.'}
          </p>
        </div>
      )}
      {visible.map(({ source, check_error, unchecked, rows }) => (
        <section
          className="store-update-group"
          key={source.store_url}
          aria-busy={retrying_store === source.store_url || undefined}
        >
          <h2 tabIndex={-1}>{source.store_url}</h2>
          {(unchecked > 0 || check_error) && (
            <div className="store-feedback" role="status">
              <strong>
                {unchecked === 1
                  ? 'Couldn’t check one applet'
                  : `Couldn’t check ${unchecked} applets`}
              </strong>
              {check_error && <p>{check_error}</p>}
              <button
                type="button"
                aria-label={`Retry updates from ${source.store_url}`}
                disabled={loading || !!retrying_store}
                onClick={(event) => {
                  const button = event.currentTarget;
                  button.focus({ preventScroll: true });
                  retry_focus.current = {
                    button,
                    heading: button.closest('section')?.querySelector('h2'),
                  };
                  onRetrySource(source.store_url);
                }}
              >
                {retrying_store === source.store_url ? 'Checking this Store…' : 'Retry this Store'}
              </button>
            </div>
          )}
          {!!rows.length && (
            <ul className="store-list">
              {rows.map((row) => {
                const { applet } = row;
                const installed_source = applet.source;
                if (installed_source?.kind !== 'store') return null;
                const channel = storeChannelName(installed_source.channel);
                const version_id = `store-update-version-${applet.applet_id}`;
                const action =
                  'reason' in row
                    ? 'View installed'
                    : row.saved
                      ? 'Check saved release for'
                      : 'Review update for';
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
                      aria-label={`${action} ${applet.name}`}
                      aria-describedby={version_id}
                    >
                      <span className="store-icon" aria-hidden="true">
                        <AppletIcon applet={applet} />
                      </span>
                      <span className="store-card-copy">
                        <strong>{applet.name}</strong>
                        <span className="store-card-meta" id={version_id}>
                          <span className="store-badge">{channel}</span>
                          {'reason' in row ? (
                            <>
                              <span>Installed {applet.version}</span>
                              <span>
                                {row.reason === 'release_unavailable'
                                  ? `No ${channel} release`
                                  : 'Update status unknown'}
                              </span>
                            </>
                          ) : (
                            <>
                              {row.saved && (
                                <span className="store-badge store-badge-muted">Saved result</span>
                              )}
                              {applet.version} → {row.version}
                            </>
                          )}
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
