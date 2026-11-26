import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { AppletSession, StorePendingDownload, StoreSwitchSnapshot } from '../lib/types';
import { storeChannelName } from '../lib/storeState';
import { useBackAction } from '../lib/backNavigation';
import { AppletIcon } from '../components/AppletIcon';
import { RestorePackageSheet } from '../components/RestorePackageSheet';
import { Sheet } from '../components/Sheet';
import { StoreAddress } from '../components/store/StoreAddress';
import { StoreCatalogView } from '../components/store/StoreCatalogView';
import { StoreDetailView, StoreOpenButton } from '../components/store/StoreDetailView';
import { StoreDownloadsView } from '../components/store/StoreDownloadsView';
import { storeDownloadId } from '../components/store/StorePendingDownloadNotice';
import { useStore } from '../components/store/useStore';
import type { StoreDownloads } from '../components/store/useStoreDownloads';
import './store.css';

type RepairDownload = StorePendingDownload & { restore_snapshot: StoreSwitchSnapshot };

export function Store({
  onLaunch,
  onBusyChange,
  downloads,
  downloads_request,
  onReturnToLibrary,
}: {
  onLaunch: (session: AppletSession) => void;
  onBusyChange: (busy: boolean) => void;
  downloads: StoreDownloads;
  downloads_request: number;
  onReturnToLibrary: (store_scroll: number) => void;
}) {
  const [downloads_open, setDownloadsOpen] = useState(false);
  const [downloads_from_library, setDownloadsFromLibrary] = useState(false);
  const [download_return_id, setDownloadReturnId] = useState<string>();
  const [repair, setRepair] = useState<RepairDownload>();
  const [repair_status, setRepairStatus] = useState('');
  const downloads_request_handled = useRef(0);
  const downloads_requested = downloads_request > downloads_request_handled.current;
  const state = useStore(onLaunch, onBusyChange, downloads.refresh, {
    downloads_open,
    downloads_requested,
  });
  const downloads_heading = useRef<HTMLHeadingElement>(null);
  const download_return = useRef<HTMLButtonElement>(null);
  const download_review = useRef<HTMLDivElement>(null);
  const repair_focus = useRef<'download' | 'heading'>(undefined);
  const { query, updates_only } = state.browse;
  const opening = state.operation === 'open' || state.operation === 'restart';
  const installed_source = state.installed?.source;
  const reviewing_download = state.reviewing_download;
  const paused_download_missing =
    reviewing_download &&
    !state.operation &&
    !downloads.loading &&
    !downloads.error &&
    !downloads.entries.some(
      (entry) => storeDownloadId(entry) === storeDownloadId(reviewing_download),
    );
  const page = state.address_open
    ? 'address'
    : state.selected_id
      ? 'detail'
      : downloads_open
        ? 'downloads'
        : updates_only
          ? 'updates'
          : 'discover';
  const previous_page = useRef(page);
  const previous_origin = useRef(state.config?.store_url);
  const list_scroll = useRef({ discover: 0, updates: 0, downloads: 0 });
  useLayoutEffect(() => {
    if (previous_origin.current !== state.config?.store_url) {
      previous_origin.current = state.config?.store_url;
      list_scroll.current.discover = 0;
    }
    const previous = previous_page.current;
    if (previous === page) return;
    previous_page.current = page;
    window.scrollTo(0, page === 'address' || page === 'detail' ? 0 : list_scroll.current[page]);
    if (page === 'downloads') {
      const entry = previous === 'detail' ? download_return.current : null;
      const target = entry && !entry.disabled ? entry : downloads_heading.current;
      target?.focus({ preventScroll: true });
    } else if (page === 'detail' && downloads_open) {
      download_review.current?.focus({ preventScroll: true });
    } else if (previous === 'downloads') {
      document.getElementById('store-open-downloads')?.focus({ preventScroll: true });
    }
  }, [page, downloads_open, state.config?.store_url]);
  useEffect(() => {
    if (repair || !repair_focus.current) return;
    const entry = repair_focus.current === 'download' ? download_return.current : null;
    repair_focus.current = undefined;
    const target = entry && !entry.disabled ? entry : downloads_heading.current;
    target?.focus({ preventScroll: true });
  }, [repair]);

  const rememberListScroll = () => {
    if (page !== 'address' && page !== 'detail') list_scroll.current[page] = window.scrollY;
  };
  const openDownloads = (from_library = false) => {
    if (state.operation) return;
    state.setAddressOpen(false);
    state.closeDetail();
    list_scroll.current.downloads = 0;
    setDownloadReturnId(undefined);
    setRepairStatus('');
    setDownloadsFromLibrary(from_library);
    setDownloadsOpen(true);
    downloads.refresh();
    if (page === 'downloads') downloads_heading.current?.focus({ preventScroll: true });
  };
  const closeDownloads = () => {
    if (state.operation) return;
    state.closeDetail();
    setDownloadsOpen(false);
    if (downloads_from_library) {
      setDownloadsFromLibrary(false);
      onReturnToLibrary(list_scroll.current[updates_only ? 'updates' : 'discover']);
    }
  };

  useLayoutEffect(() => {
    if (state.operation || downloads_request <= downloads_request_handled.current) return;
    downloads_request_handled.current = downloads_request;
    openDownloads(true);
  }, [downloads_request, state.operation]);

  useBackAction(25, state.address_open || !!state.selected_id || downloads_open, () => {
    if (state.operation) return;
    if (state.address_open) state.setAddressOpen(false);
    else if (state.selected_id) state.closeDetail();
    else closeDownloads();
  });
  if (state.initial_loading && !downloads_open)
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
      {page === 'downloads' && repair_status && (
        <p className="store-feedback store-success" role="status">
          {repair_status}
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
      {paused_download_missing && (
        <p className="store-feedback" role="alert">
          This paused download is no longer available.
        </p>
      )}
      {state.selected_id ? (
        <div
          ref={download_review}
          tabIndex={downloads_open ? -1 : undefined}
          role={downloads_open ? 'region' : undefined}
          aria-label={downloads_open ? 'Download review' : undefined}
          className="store-detail-route"
        >
          {state.detail && state.review ? (
            <StoreDetailView
              detail={state.detail}
              installed_source={state.installed_source}
              reviewing_download={reviewing_download}
              restart_required={state.restart_required}
              review={state.review}
              fresh={state.detail_fresh && !paused_download_missing}
              refreshing={state.detail_loading}
              busy={!!state.operation}
              installing={state.operation === 'install'}
              opening={opening}
              progress={state.progress}
              transfer_control={state.transfer_control}
              onChannel={state.setChannel}
              onVersion={state.setSelectedVersion}
              onInstall={state.install}
              onOpen={state.openApplet}
              onRefresh={state.retryDetail}
              onBack={state.closeDetail}
              back_label={downloads_open ? 'Back to downloads' : undefined}
            />
          ) : (
            <section className="store-detail-loading" aria-busy={state.detail_loading}>
              <button
                className="store-back"
                disabled={!!state.operation}
                onClick={state.closeDetail}
              >
                {downloads_open
                  ? 'Back to downloads'
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
                  {reviewing_download ? 'Checking this release…' : 'Loading applet…'}
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
                      {storeChannelName(reviewing_download.channel)} ·{' '}
                      <bdi>{reviewing_download.origin}</bdi>
                    </p>
                  )}
                  <p>{state.detail_error}</p>
                  <button disabled={!!state.operation} onClick={state.retryDetail}>
                    Try again
                  </button>
                </div>
              )}
            </section>
          )}
        </div>
      ) : downloads_open ? (
        <StoreDownloadsView
          downloads={downloads}
          heading_ref={downloads_heading}
          return_id={download_return_id}
          return_ref={download_return}
          reviews_disabled={downloads.loading || !!state.operation}
          onReview={(pending) => {
            rememberListScroll();
            setDownloadReturnId(storeDownloadId(pending));
            setRepairStatus('');
            if (pending.restore_snapshot) {
              state.closeDetail();
              setRepair({ ...pending, restore_snapshot: pending.restore_snapshot });
            } else {
              state.reviewDownload(pending);
            }
          }}
        />
      ) : (
        <StoreCatalogView
          catalog={state.catalog}
          store_url={state.config?.store_url}
          updates={state.updates}
          download_count={downloads.entries.length}
          loading={updates_only ? state.updates_loading : state.catalog_loading}
          error={updates_only ? state.updates_error : state.config_error || state.catalog_error}
          query={updates_only ? state.updates_query : query}
          updates_only={updates_only}
          onQuery={updates_only ? state.setUpdatesQuery : (query) => state.updateBrowse({ query })}
          onFilter={(next) => {
            if (next === updates_only) return;
            rememberListScroll();
            state.updateBrowse({ updates_only: next });
          }}
          onRefresh={updates_only ? state.refreshUpdates : state.refreshCatalog}
          onAddress={() => {
            rememberListScroll();
            state.setAddressOpen(true);
          }}
          onSelect={(applet_id) => {
            rememberListScroll();
            state.openDetail(applet_id);
          }}
          onSelectUpdate={(source) => {
            rememberListScroll();
            state.openUpdate(source);
          }}
          onOpenDownloads={() => {
            rememberListScroll();
            openDownloads();
          }}
        />
      )}
      {reviewing_download && !paused_download_missing && (
        <DiscardDownload
          name={reviewing_download.name}
          version={reviewing_download.version}
          disabled={!!state.operation}
          onDiscard={() => {
            setDownloadReturnId(undefined);
            state.discardDownload();
          }}
        />
      )}
      {repair && (
        <RestorePackageSheet
          target={{
            applet_id: repair.applet_id,
            name: repair.name,
            expected: repair.restore_snapshot,
          }}
          downloads={downloads}
          pending_download={repair}
          onBusyChange={onBusyChange}
          onClose={(reason) => {
            repair_focus.current = reason === 'discarded' ? 'heading' : 'download';
            setRepair(undefined);
          }}
          onRestored={({ applet, restart_required }) => {
            const summary = `${applet.name} ${applet.version} restored.`;
            setRepairStatus(
              restart_required ? `${summary} Restart it from Library to use it.` : summary,
            );
            repair_focus.current = 'heading';
            setRepair(undefined);
          }}
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
