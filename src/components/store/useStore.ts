import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  discardStoreDownload,
  getSavedStoreCatalog,
  getSavedStoreDetail,
  getSavedStoreUpdates,
  getStoreCatalog,
  getStoreConfig,
  getStoreDetail,
  getStoreDownloadDetail,
  getStoreUpdates,
  installStoreRelease,
  launchApplet,
  listApplets,
  message,
  onAppletInstallationChanged,
  restartApplet,
  setStoreUrl,
} from '../../lib/tauri';
import type {
  AppletInstallResult,
  AppletSession,
  StoreCatalog,
  StoreChannel,
  StoreConfig,
  StoreDetail,
  StorePendingDownload,
  StoreReleaseOption,
  StoreSource,
  StoreUpdateResults,
} from '../../lib/types';
import {
  installedStoreChannel,
  reviewSavedCatalog,
  reviewStoreRelease,
  storeChannelName,
  storeReleaseChannel,
  type StoreReleaseChoice,
} from '../../lib/storeState';
import {
  DEFAULT_BROWSE,
  readStorePreferences,
  rememberedStoreVersion,
  rememberStoreVersion,
  writeStorePreferences,
  type StoreBrowsePreferences,
} from './cache';
import { useStoreTransfer } from './useStoreTransfer';

type StoreOperation = 'save' | 'install' | 'open' | 'restart' | 'discard';
interface DetailTarget {
  store_url: string;
  applet_id: string;
  installed_source?: boolean;
}
interface DetailSelection extends StoreReleaseChoice {
  target: DetailTarget;
}

export function useStore(
  onLaunch: (session: AppletSession) => void,
  onBusyChange: (busy: boolean) => void,
  refreshDownloads: () => void,
  {
    downloads_open,
    downloads_requested,
  }: { downloads_open: boolean; downloads_requested: boolean },
) {
  const [config, setConfig] = useState<StoreConfig>();
  const [browse, setBrowse] = useState(DEFAULT_BROWSE);
  const [catalog, setCatalog] = useState<StoreCatalog>();
  const [updates, setUpdates] = useState<StoreUpdateResults>();
  const [updates_query, setUpdatesQuery] = useState('');
  const [detail, setDetail] = useState<StoreDetail>();
  const [local_installation, setLocalInstallation] = useState<AppletInstallResult>();
  const [detail_selection, setDetailSelection] = useState<DetailSelection>();
  const [address_open, setAddressOpen] = useState(false);
  const [config_check, setConfigCheck] = useState<number>();
  const [list_refresh, setListRefresh] = useState(0);
  const [detail_refresh, setDetailRefresh] = useState(0);
  const [catalog_loading, setCatalogLoading] = useState(false);
  const [updates_loading, setUpdatesLoading] = useState(false);
  const [detail_loading, setDetailLoading] = useState(false);
  const [detail_fresh, setDetailFresh] = useState(false);
  const [config_error, setConfigError] = useState('');
  const [catalog_error, setCatalogError] = useState('');
  const [updates_error, setUpdatesError] = useState('');
  const [detail_error, setDetailError] = useState('');
  const [status, setStatus] = useState('');
  const [action_error, setActionError] = useState('');
  const [operation, setOperation] = useState<StoreOperation>();
  const transfer = useStoreTransfer();
  const scope = useRef({
    alive: false,
    config: 0,
    catalog: 0,
    updates: 0,
    detail: 0,
    operation: '',
  });
  const list_checked = useRef({ catalog: false, updates: false });
  const current_url = useRef<string | null>(null);
  const selection = useRef<DetailSelection | undefined>(undefined);
  const fresh = useRef<StoreDetail>(undefined);
  const deferred_detail = useRef<DetailTarget>(undefined);

  const selectDetail = (next?: DetailSelection) => {
    selection.current = next;
    setDetailSelection(next);
  };

  const markFresh = (snapshot?: StoreDetail) => {
    fresh.current = snapshot;
    setDetailFresh(!!snapshot);
  };

  const adoptStore = (store_url: string | null) => {
    if (current_url.current === store_url) return;
    current_url.current = store_url;
    scope.current.catalog++;
    list_checked.current.catalog = false;
    const preferences = store_url ? readStorePreferences(store_url) : DEFAULT_BROWSE;
    setBrowse((current) => {
      return current.updates_only ? { ...preferences, updates_only: true } : preferences;
    });
    setCatalog(undefined);
    setCatalogError('');
    setCatalogLoading(false);
    const current = selection.current;
    if (current && !current.target.installed_source && !current.pending_download) closeDetail();
  };

  const updateBrowse = (patch: Partial<StoreBrowsePreferences>) => {
    const next = {
      ...browse,
      ...patch,
      ...(patch.query !== undefined ? { query: patch.query.slice(0, 200) } : {}),
    };
    setBrowse(next);
    if (current_url.current) writeStorePreferences(current_url.current, next);
  };

  const isCurrentCatalog = (store_url: string, request: number) =>
    scope.current.alive && scope.current.catalog === request && current_url.current === store_url;

  const clearCatalogReview = () => setCatalog((current) => current && reviewSavedCatalog(current));

  const markUpdatesSaved = (changed_applet_id?: string) =>
    setUpdates(
      (current) =>
        current && {
          fresh: false,
          sources: changed_applet_id
            ? current.sources.map((source) => ({
                ...source,
                updates: source.updates.filter(
                  ({ applet }) => applet.applet_id !== changed_applet_id,
                ),
                saved_updates: source.saved_updates.filter(
                  ({ applet }) => applet.applet_id !== changed_applet_id,
                ),
              }))
            : current.sources,
        },
    );

  const invalidateLists = (changed_applet_id?: string) => {
    scope.current.catalog++;
    scope.current.updates++;
    list_checked.current = { catalog: false, updates: false };
    setCatalogLoading(false);
    setUpdatesLoading(false);
    clearCatalogReview();
    markUpdatesSaved(changed_applet_id);
  };

  const loadCatalog = async (store_url: string) => {
    list_checked.current.catalog = true;
    const request = ++scope.current.catalog;
    setCatalogLoading(true);
    setCatalogError('');
    clearCatalogReview();
    let checked = false;
    const saved = (async () => {
      try {
        const result = await getSavedStoreCatalog(store_url);
        if (result && !checked && isCurrentCatalog(store_url, request)) setCatalog(result);
      } catch {
        // The live check can still succeed when saved listings are unavailable.
      }
    })();
    try {
      const result = await getStoreCatalog(store_url);
      checked = true;
      if (isCurrentCatalog(store_url, request)) setCatalog(result);
    } catch (error) {
      await saved;
      if (isCurrentCatalog(store_url, request)) setCatalogError(message(error));
    } finally {
      if (isCurrentCatalog(store_url, request)) setCatalogLoading(false);
    }
  };

  const loadUpdates = async () => {
    list_checked.current.updates = true;
    const request = ++scope.current.updates;
    const isCurrent = () => scope.current.alive && scope.current.updates === request;
    setUpdatesLoading(true);
    setUpdatesError('');
    markUpdatesSaved();
    let checked = false;
    const saved = (async () => {
      try {
        const result = await getSavedStoreUpdates();
        if (isCurrent() && !checked) setUpdates({ ...result, fresh: false });
      } catch {
        // A saved result is optional. The live check still runs.
      }
    })();
    try {
      const result = await getStoreUpdates();
      checked = true;
      if (isCurrent()) setUpdates({ ...result, fresh: true });
    } catch (error) {
      await saved;
      if (!isCurrent()) return;
      setUpdatesError(message(error));
    } finally {
      if (isCurrent()) setUpdatesLoading(false);
    }
  };

  const loadDetail = async ({ target, pending_download }: DetailSelection) => {
    const { store_url, applet_id, installed_source } = target;
    const request = ++scope.current.detail;
    const isCurrent = () =>
      scope.current.alive &&
      scope.current.detail === request &&
      selection.current?.target === target;
    markFresh();
    setDetailLoading(true);
    setDetailError('');
    void listApplets().then(
      (report) => {
        if (isCurrent()) {
          const applet = report.applets.find(
            (applet) =>
              applet.applet_id === applet_id &&
              (!installed_source || installedStoreChannel(store_url, applet_id, applet)),
          );
          setLocalInstallation(
            applet && { applet, restart_required: report.restart_required.includes(applet_id) },
          );
        }
      },
      () => {},
    );
    let checked = false;
    const saved = pending_download
      ? undefined
      : (async () => {
          try {
            const result = await getSavedStoreDetail(store_url, applet_id, installed_source);
            if (result && !checked && isCurrent()) {
              setDetail({
                ...result,
                releases: result.releases.map((release) => ({ ...release, action: 'conflict' })),
                installed: null,
                restart_required: false,
              });
            }
          } catch {
            // A previously shown installed-source page may no longer belong to this Store.
            if (installed_source && !checked && isCurrent()) setDetail(undefined);
          }
        })();
    try {
      const result = await (pending_download
        ? getStoreDownloadDetail(pending_download)
        : getStoreDetail(store_url, applet_id, installed_source));
      checked = true;
      const requested = selection.current;
      if (isCurrent() && requested) {
        setDetail(result);
        markFresh(result);
        if (!installed_source && !pending_download) {
          const remembered = requested.version ?? requested.remembered_version;
          if (requested.channel || remembered !== undefined) {
            const version = requested.channel
              ? reviewStoreRelease(result, requested).release?.version
              : result.releases.find((release) => release.version === remembered)?.version;
            rememberStoreVersion(store_url, applet_id, version);
          }
        }
      }
    } catch (error) {
      await saved;
      if (isCurrent()) setDetailError(message(error));
    } finally {
      if (isCurrent()) setDetailLoading(false);
    }
  };

  const loadConfig = async () => {
    const request = ++scope.current.config;
    setConfigCheck(undefined);
    setConfigError('');
    try {
      const result = await getStoreConfig();
      if (!scope.current.alive || scope.current.config !== request) return;
      adoptStore(result.store_url);
      setConfig(result);
    } catch (error) {
      if (scope.current.alive && scope.current.config === request) {
        setConfigError(message(error));
      }
    } finally {
      if (scope.current.alive && scope.current.config === request) {
        setConfigCheck(request);
      }
    }
  };

  useLayoutEffect(() => {
    scope.current.alive = true;
    markFresh();
    invalidateLists();
    if (selection.current) {
      setLocalInstallation(undefined);
      setDetailLoading(true);
      setDetailError('');
    }
    return () => {
      scope.current.alive = false;
      fresh.current = undefined;
      scope.current.config++;
      scope.current.catalog++;
      scope.current.updates++;
      scope.current.detail++;
      deferred_detail.current = undefined;
    };
  }, []);
  useEffect(() => {
    void loadConfig();
  }, []);
  useEffect(() => {
    let open = true;
    const subscription = onAppletInstallationChanged((applet_id) => {
      if (!open || !scope.current.alive) return;
      invalidateLists(applet_id);
      setListRefresh((revision) => revision + 1);
      const target = selection.current?.target;
      if (target?.applet_id !== applet_id) return;
      scope.current.detail++;
      markFresh();
      setLocalInstallation(undefined);
      setDetailLoading(true);
      setDetailError('');
      if (scope.current.operation) deferred_detail.current = target;
      else setDetailRefresh((revision) => revision + 1);
    }).catch(() => undefined);
    return () => {
      open = false;
      void subscription.then((unlisten) => unlisten?.());
    };
  }, []);

  // The Library shortcut can replace the previously visible Store route on reveal.
  const show_list =
    config_check !== undefined &&
    !address_open &&
    !detail_selection &&
    !downloads_open &&
    !downloads_requested;
  const show_updates = show_list && browse.updates_only;
  const discover_url =
    show_list && !browse.updates_only && !config_error ? config?.store_url : null;
  useEffect(() => {
    if (scope.current.config !== config_check) return;
    if (show_updates && !list_checked.current.updates) void loadUpdates();
    if (discover_url && !list_checked.current.catalog) void loadCatalog(discover_url);
  }, [show_updates, discover_url, config_check, list_refresh]);

  const configured_detail =
    !detail_selection?.target.installed_source && !detail_selection?.pending_download;
  const visible_detail =
    !address_open &&
    !downloads_requested &&
    detail_selection &&
    (!configured_detail || config_check !== undefined)
      ? detail_selection.target
      : undefined;
  const detail_config_check = configured_detail ? config_check : undefined;
  useEffect(() => {
    const selected = selection.current;
    if (!selected || selected.target !== visible_detail) return;
    if (configured_detail && scope.current.config !== detail_config_check) return;
    void loadDetail(selected);
  }, [visible_detail, detail_config_check, detail_refresh]);

  const run = async (kind: StoreOperation, action: (id: string) => Promise<void>) => {
    if (scope.current.operation) return;
    const id = crypto.randomUUID();
    scope.current.operation = id;
    setOperation(kind);
    setActionError('');
    setStatus('');
    onBusyChange(true);
    try {
      await action(id);
    } catch (error) {
      if (scope.current.alive) setActionError(message(error));
    } finally {
      if (scope.current.operation === id) {
        scope.current.operation = '';
        const target = deferred_detail.current;
        deferred_detail.current = undefined;
        if (target && target === selection.current?.target) {
          setDetailRefresh((revision) => revision + 1);
        }
        onBusyChange(false);
        setOperation(undefined);
      }
    }
  };

  const closeDetail = () => {
    scope.current.detail++;
    selectDetail();
    markFresh();
    setDetail(undefined);
    setLocalInstallation(undefined);
    setDetailError('');
    setDetailLoading(false);
    setActionError('');
    setStatus('');
  };

  const openReview = (requested: DetailSelection) => {
    if (scope.current.operation) return;
    scope.current.detail++;
    setActionError('');
    setStatus('');
    selectDetail(requested);
    markFresh();
    setDetail(undefined);
    setLocalInstallation(undefined);
    setDetailLoading(true);
    setDetailError('');
  };

  const reviewDownload = (pending_download: StorePendingDownload) =>
    openReview({
      target: { store_url: pending_download.origin, applet_id: pending_download.applet_id },
      pending_download,
    });

  const discardDownload = () => {
    const pending = selection.current?.pending_download;
    if (!pending) return;
    void run('discard', async () => {
      try {
        const discarded = await discardStoreDownload(pending);
        if (!scope.current.alive) return;
        closeDetail();
        setStatus(
          discarded ? 'Download discarded.' : 'This paused download is no longer available.',
        );
      } finally {
        refreshDownloads();
      }
    });
  };

  const saveAddress = (value: string) =>
    void run('save', async () => {
      const request = ++scope.current.config;
      try {
        const result = await setStoreUrl(value.trim());
        if (!scope.current.alive || scope.current.config !== request) return;
        adoptStore(result.store_url);
        setConfig(result);
        setConfigError('');
        setAddressOpen(false);
      } finally {
        if (scope.current.alive && scope.current.config === request) {
          setConfigCheck(request);
        }
      }
    });

  const install = (release: StoreReleaseOption, confirmed_channel_switch = false) => {
    const requested = selection.current;
    const snapshot = fresh.current;
    if (!snapshot || snapshot !== detail || !requested) return;
    const { target } = requested;
    if (reviewStoreRelease(snapshot, requested, target.installed_source).release !== release)
      return;
    const resume = !!requested.pending_download;
    const has_checkpoint = resume || (release.downloaded_bytes ?? 0) > 0;
    const switching = release.action === 'switch_source';
    const switching_channel = release.action === 'switch_channel';
    if (target.installed_source) {
      const same_channel_update =
        release.action === 'update' && storeReleaseChannel(release) === snapshot.installed_channel;
      if (!same_channel_update) return;
    } else if (!resume && target.store_url !== current_url.current) return;
    if (switching && !snapshot.switch_snapshot) return;
    if (switching_channel && (!confirmed_channel_switch || !snapshot.channel_snapshot)) return;
    if (!switching && !switching_channel && !['install', 'update'].includes(release.action)) return;
    const review_revision = scope.current.detail;
    void run('install', async (operation_id) => {
      try {
        const installed = await transfer.run(operation_id, async () => {
          if (!scope.current.alive || fresh.current !== snapshot || selection.current !== requested)
            return;
          return installStoreRelease({
            store_url: snapshot.store_url,
            applet_id: snapshot.applet.applet_id,
            release,
            expected_installed_sha256: snapshot.installed?.sha256 ?? null,
            ...(switching ? { switch_snapshot: snapshot.switch_snapshot } : {}),
            ...(switching_channel ? { channel_snapshot: snapshot.channel_snapshot } : {}),
            ...(target.installed_source ? { installed_source: true } : {}),
            ...(has_checkpoint ? { resume_download: true } : {}),
            operation_id,
          });
        });
        if (!installed || !scope.current.alive) return;
        if (installed.status === 'paused') {
          if (resume && installed.received_bytes === 0) {
            closeDetail();
            setStatus(
              'The download stopped and no saved progress remains. Open the applet in the Store to start again.',
            );
            return;
          }
          if (scope.current.detail === review_revision) {
            const paused = {
              ...snapshot,
              releases: snapshot.releases.map((option) =>
                option === release
                  ? { ...option, downloaded_bytes: installed.received_bytes || undefined }
                  : option,
              ),
            };
            setDetail(paused);
            markFresh(paused);
          }
          setStatus(
            installed.received_bytes > 0
              ? 'Download paused. Continue when you’re ready.'
              : 'Download paused. Start again when you’re ready.',
          );
          return;
        }
        setStatus(
          switching
            ? 'Switched to this store.'
            : switching_channel
              ? `Switched to ${storeChannelName(storeReleaseChannel(release))}.`
              : 'Installed.',
        );
        const installed_source =
          target.installed_source || (resume && snapshot.store_url !== current_url.current);
        const next_selection: DetailSelection = {
          ...requested,
          target: { ...target, installed_source },
          pending_download: undefined,
          version: requested.pending_download?.version ?? selection.current?.version,
          channel: installed_source ? undefined : storeReleaseChannel(release),
        };
        markFresh();
        setLocalInstallation(installed);
        selectDetail(next_selection);
        invalidateLists(installed.applet.applet_id);
      } catch (error) {
        if (has_checkpoint && scope.current.alive && scope.current.detail === review_revision) {
          void loadDetail(requested);
        }
        throw error;
      } finally {
        refreshDownloads();
      }
    });
  };

  const openInstalled = (applet_id: string, restart: boolean) =>
    void run(restart ? 'restart' : 'open', async () => {
      const launch = restart ? restartApplet : launchApplet;
      const session = await launch(applet_id);
      if (scope.current.alive) onLaunch(session);
    });

  const installed = detail_fresh ? detail?.installed : local_installation?.applet;
  const restart_required = Boolean(
    detail_fresh ? detail?.restart_required : local_installation?.restart_required,
  );
  const displayed_detail =
    detail &&
    (detail_fresh
      ? detail
      : { ...detail, installed: installed ?? null, installed_channel: undefined });
  const chooseRelease = (choice: StoreReleaseChoice) => {
    const requested = selection.current;
    if (
      !displayed_detail ||
      !requested ||
      requested.target.installed_source ||
      requested.pending_download
    )
      return;
    const next = { ...requested, ...choice, remembered_version: undefined };
    selectDetail(next);
    rememberStoreVersion(
      requested.target.store_url,
      requested.target.applet_id,
      reviewStoreRelease(displayed_detail, choice).release?.version,
    );
  };

  return {
    config,
    browse,
    updateBrowse,
    catalog,
    updates,
    updates_query,
    setUpdatesQuery,
    detail: displayed_detail,
    reviewing_download: detail_selection?.pending_download,
    reviewDownload,
    discardDownload,
    installed,
    restart_required,
    selected_id: detail_selection?.target.applet_id,
    review:
      displayed_detail &&
      reviewStoreRelease(
        displayed_detail,
        detail_selection,
        detail_selection?.target.installed_source,
      ),
    setChannel: (channel: StoreChannel) => chooseRelease({ channel, version: undefined }),
    setSelectedVersion: (version: string) => {
      const release = displayed_detail?.releases.find((option) => option.version === version);
      if (release) chooseRelease({ version, channel: storeReleaseChannel(release) });
    },
    address_open,
    setAddressOpen,
    initial_loading: config_check === undefined && !config,
    catalog_loading: catalog_loading || (!!discover_url && !list_checked.current.catalog),
    updates_loading,
    detail_loading,
    detail_fresh,
    config_error,
    catalog_error,
    updates_error,
    detail_error,
    status,
    action_error,
    operation,
    progress: transfer.progress,
    transfer_control: transfer.control,
    saveAddress,
    closeDetail: () => {
      if (!scope.current.operation) closeDetail();
    },
    install,
    installed_source: detail_selection?.target.installed_source === true,
    openDetail: (applet_id: string) => {
      if (!current_url.current || scope.current.operation) return;
      const store_url = current_url.current;
      const listing = catalog?.applets.find((entry) => entry.applet_id === applet_id);
      const use_latest = listing?.update_available && !listing.source_conflict;
      const remembered_version = use_latest
        ? undefined
        : rememberedStoreVersion(store_url, applet_id);
      openReview({ target: { store_url, applet_id }, remembered_version });
    },
    openUpdate: (source: StoreSource) =>
      openReview({
        target: {
          store_url: source.store_url,
          applet_id: source.applet_id,
          installed_source: true,
        },
      }),
    refreshCatalog: () => {
      if (scope.current.operation || scope.current.config !== config_check) return;
      if (!config || config_error) {
        list_checked.current.catalog = false;
        void loadConfig();
      } else if (current_url.current) void loadCatalog(current_url.current);
    },
    refreshUpdates: () => {
      if (scope.current.operation) return;
      void loadUpdates();
    },
    retryDetail: () => {
      if (!selection.current || scope.current.operation) return;
      void loadDetail(selection.current);
    },
    openApplet: () => {
      if (installed && selection.current?.target.applet_id === installed.applet_id) {
        openInstalled(installed.applet_id, restart_required);
      }
    },
  };
}
