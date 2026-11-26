import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import type {
  AppletSession,
  InstalledApplet,
  StorePendingDownload,
  StoreSwitchSnapshot,
} from '../lib/types';
import { installedStoreChannel, storeChannelName } from '../lib/storeState';
import { useBackAction } from '../lib/backNavigation';
import { AppletIcon } from '../components/AppletIcon';
import { InstalledDevicePermissionsSheet } from '../components/InstalledDevicePermissionsSheet';
import { RestorePackageSheet } from '../components/RestorePackageSheet';
import { Sheet } from '../components/Sheet';
import { StoreAddress } from '../components/store/StoreAddress';
import { StoreCatalogView } from '../components/store/StoreCatalogView';
import { StoreDetailView, StoreOpenButton } from '../components/store/StoreDetailView';
import { StoreDownloadsView } from '../components/store/StoreDownloadsView';
import { storeDownloadId } from '../components/store/StorePendingDownloadNotice';
import { useStore } from '../components/store/useStore';
import type { StoreDownloads } from '../components/store/useStoreDownloads';
import type { StoreUpdates } from '../components/store/useStoreUpdates';
import './store.css';

type RepairDownload = StorePendingDownload & { restore_snapshot: StoreSwitchSnapshot };
type BrowsePage = 'discover' | 'updates';
type BrowseCardsReturn = { page: BrowsePage; kind: 'cards'; keys: string[]; top: number };
type BrowseReturn = BrowseCardsReturn | { page: BrowsePage; kind: 'address'; trigger_id: string };

function storeCards(catalog: HTMLElement | null) {
  return Array.from(catalog?.querySelectorAll<HTMLButtonElement>('button[data-store-card]') ?? []);
}

function alignStoreCard(card: HTMLButtonElement, top: number) {
  const offset = card.getBoundingClientRect().top - top;
  if (Math.abs(offset) > 1) window.scrollTo(0, window.scrollY + offset);
  card.scrollIntoView({ block: 'nearest' });
}

export function Store({
  onLaunch,
  onBusyChange,
  downloads,
  updates,
  downloads_request,
  onReturnToLibrary,
}: {
  onLaunch: (session: AppletSession) => void;
  onBusyChange: (busy: boolean) => void;
  downloads: StoreDownloads;
  updates: StoreUpdates;
  downloads_request: number;
  onReturnToLibrary: (store_scroll: number) => void;
}) {
  const [downloads_open, setDownloadsOpen] = useState(false);
  const [downloads_from_library, setDownloadsFromLibrary] = useState(false);
  const [download_return_id, setDownloadReturnId] = useState<string>();
  const [repair, setRepair] = useState<RepairDownload>();
  const [repair_status, setRepairStatus] = useState('');
  const [permission_applet, setPermissionApplet] = useState<InstalledApplet>();
  const downloads_request_handled = useRef(0);
  const downloads_requested = downloads_request > downloads_request_handled.current;
  const state = useStore(onLaunch, onBusyChange, downloads.refresh, updates, {
    downloads_open,
    downloads_requested,
  });
  const downloads_heading = useRef<HTMLHeadingElement>(null);
  const download_return = useRef<HTMLButtonElement>(null);
  const detail_route = useRef<HTMLDivElement>(null);
  const permission_return = useRef<HTMLButtonElement>(null);
  const address_heading = useRef<HTMLHeadingElement>(null);
  const catalog = useRef<HTMLElement>(null);
  const browse_return = useRef<BrowseReturn>(undefined);
  const returned_card = useRef<{
    location: BrowseCardsReturn;
    button: HTMLButtonElement;
    scroll: number;
  }>(undefined);
  const repair_focus = useRef<'download' | 'heading'>(undefined);
  const { query, updates_only } = state.browse;
  const opening = state.operation === 'open' || state.operation === 'restart';
  const installed_source = state.installed?.source;
  const reviewing_download = state.reviewing_download;
  const installed = state.installed;
  const can_manage_permissions =
    !reviewing_download &&
    installed?.applet_id === state.selected_id &&
    state.selected_id &&
    state.selected_store_url &&
    installedStoreChannel(state.selected_store_url, state.selected_id, installed ?? null);
  const permissions_action = can_manage_permissions ? (
    <button
      type="button"
      id="store-installed-permissions"
      aria-haspopup="dialog"
      disabled={!!state.operation}
      onClick={(event) => {
        if (!installed || state.operation) return;
        event.currentTarget.focus({ preventScroll: true });
        permission_return.current = event.currentTarget;
        setPermissionApplet(installed);
      }}
    >
      Device permissions
    </button>
  ) : undefined;
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
  const stopTrackingCard = () => {
    returned_card.current = undefined;
    catalog.current?.removeAttribute('data-return-anchor');
  };
  const restoreCard = (location: BrowseCardsReturn, restore_position = true) => {
    const cards = storeCards(catalog.current);
    const button = location.keys
      .map((key) => cards.find((card) => card.dataset.storeCard === key && !card.disabled))
      .find(Boolean);
    if (button) {
      catalog.current?.setAttribute('data-return-anchor', '');
      if (restore_position) alignStoreCard(button, location.top);
      button.focus({ preventScroll: true });
      returned_card.current = {
        button,
        location: restore_position
          ? location
          : { ...location, top: button.getBoundingClientRect().top },
        scroll: window.scrollY,
      };
    } else {
      stopTrackingCard();
      catalog.current?.querySelector<HTMLButtonElement>('button[aria-pressed="true"]')?.focus({
        preventScroll: !restore_position,
      });
    }
  };
  useLayoutEffect(() => {
    if (previous_origin.current !== state.config?.store_url) {
      previous_origin.current = state.config?.store_url;
      list_scroll.current.discover = 0;
    }
    const previous = previous_page.current;
    if (previous === page || (state.initial_loading && !downloads_open)) return;
    previous_page.current = page;
    stopTrackingCard();
    window.scrollTo(0, page === 'address' || page === 'detail' ? 0 : list_scroll.current[page]);
    if (page === 'downloads') {
      const entry = previous === 'detail' ? download_return.current : null;
      const target = entry && !entry.disabled ? entry : downloads_heading.current;
      target?.focus({ preventScroll: true });
    } else if (page === 'detail') {
      detail_route.current?.focus({ preventScroll: true });
    } else if (page === 'address') {
      address_heading.current?.focus({ preventScroll: true });
    } else if (previous === 'downloads') {
      document.getElementById('store-open-downloads')?.focus({ preventScroll: true });
    } else if (previous === 'detail' || previous === 'address') {
      const returning = browse_return.current;
      browse_return.current = undefined;
      if (returning?.page !== page) return;
      if (returning.kind === 'address') {
        const trigger = document.getElementById(returning.trigger_id);
        (trigger ?? document.getElementById('store-address-settings'))?.focus();
        return;
      }
      restoreCard(returning);
    }
  }, [page, downloads_open, state.config?.store_url, state.initial_loading]);
  useLayoutEffect(() => {
    const returned = returned_card.current;
    if (!returned || returned.location.page !== page) return;
    const same_scroll = Math.abs(window.scrollY - returned.scroll) <= 1;
    const focused = document.activeElement;
    // A returned list may replace saved cards or remove its loading banner.
    if (!returned.button.isConnected && focused === document.body) {
      restoreCard(returned.location, same_scroll);
    } else if (focused === returned.button && same_scroll) {
      alignStoreCard(returned.button, returned.location.top);
      returned.scroll = window.scrollY;
    } else stopTrackingCard();
  }, [
    page,
    state.catalog,
    state.catalog_loading,
    state.catalog_error,
    state.updates,
    state.updates_loading,
    state.updates_error,
  ]);
  useEffect(() => {
    if (repair || !repair_focus.current) return;
    const entry = repair_focus.current === 'download' ? download_return.current : null;
    repair_focus.current = undefined;
    const target = entry && !entry.disabled ? entry : downloads_heading.current;
    target?.focus({ preventScroll: true });
  }, [repair]);
  useEffect(() => {
    const trigger = permission_return.current;
    if (permission_applet || !trigger) return;
    permission_return.current = null;
    if (page !== 'detail' || (trigger.isConnected && !trigger.disabled)) return;
    const route = detail_route.current;
    const next = route?.querySelector<HTMLButtonElement>(
      '#store-installed-permissions:not(:disabled)',
    );
    (next ?? route)?.focus({ preventScroll: true });
  }, [permission_applet, page]);

  const rememberListScroll = () => {
    if (page !== 'address' && page !== 'detail') list_scroll.current[page] = window.scrollY;
  };
  const rememberBrowse = (trigger: HTMLButtonElement) => {
    if (page !== 'discover' && page !== 'updates') return;
    rememberListScroll();
    if (!trigger.dataset.storeCard) {
      browse_return.current = { page, kind: 'address', trigger_id: trigger.id };
      return;
    }
    const cards = storeCards(catalog.current);
    const index = cards.indexOf(trigger);
    const keys = [
      trigger.dataset.storeCard,
      cards[index + 1]?.dataset.storeCard,
      cards[index - 1]?.dataset.storeCard,
    ].filter((key): key is string => !!key);
    browse_return.current = { page, kind: 'cards', keys, top: trigger.getBoundingClientRect().top };
  };
  const openDownloads = (from_library = false) => {
    if (state.operation) return;
    state.setAddressOpen(false);
    setPermissionApplet(undefined);
    state.closeDetail();
    browse_return.current = undefined;
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
        heading_ref={address_heading}
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
          ref={detail_route}
          tabIndex={-1}
          role="region"
          aria-label={downloads_open ? 'Download review' : 'Applet details'}
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
              permissions_action={permissions_action}
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
                      {state.installed_source && installed_source?.kind === 'store' && (
                        <p>
                          <bdi>{installed_source.store_url}</bdi>
                        </p>
                      )}
                    </div>
                  </div>
                  <div className="store-install-actions">
                    <StoreOpenButton
                      restart_required={state.restart_required}
                      busy={!!state.operation}
                      opening={opening}
                      onOpen={state.openApplet}
                    />
                    {permissions_action}
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
          catalog_ref={catalog}
          catalog={state.catalog}
          store_url={state.config?.store_url}
          updates={state.updates}
          retrying_store={state.retrying_store}
          updates_notice={state.updates_notice}
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
          onRetryUpdateSource={(store_url) => void state.retryUpdateSource(store_url)}
          onAddress={(trigger) => {
            rememberBrowse(trigger);
            state.setAddressOpen(true);
          }}
          onSelect={(applet_id, trigger) => {
            rememberBrowse(trigger);
            state.openDetail(applet_id);
          }}
          onSelectUpdate={(source, trigger) => {
            rememberBrowse(trigger);
            state.openUpdate(source);
          }}
          onOpenDownloads={() => {
            rememberListScroll();
            openDownloads();
          }}
        />
      )}
      {permission_applet &&
        page === 'detail' &&
        permission_applet.applet_id === state.selected_id && (
          <InstalledDevicePermissionsSheet
            key={permission_applet.applet_id}
            applet={permission_applet}
            onClose={() => setPermissionApplet(undefined)}
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
