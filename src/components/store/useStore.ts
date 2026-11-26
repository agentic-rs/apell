import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  discardStoreDownload,
  getStoreCatalog,
  getStoreConfig,
  getStoreDetail,
  getStorePendingDownload,
  getStoreUpdates,
  installStoreRelease,
  launchApplet,
  listApplets,
  message,
  onInstallProgress,
  restartApplet,
  setStoreUrl,
} from '../../lib/tauri';
import type {
  AppletInstallResult,
  AppletSession,
  InstallProgress,
  StoreCatalog,
  StoreChannel,
  StoreConfig,
  StoreDetail,
  StorePendingDownload,
  StoreReleaseOption,
  StoreSource,
} from '../../lib/types';
import {
  reviewSavedCatalog,
  reviewStoreRelease,
  storeChannelName,
  storeReleaseChannel,
  type StoreReleaseChoice,
} from '../../lib/storeState';
import {
  cachedCatalog,
  cachedDetail,
  cacheStoreCatalog,
  cacheStoreDetail,
  DEFAULT_BROWSE,
  readStoreCache,
  writeStoreCache,
  writeStorePreferences,
  type StoreBrowseCache,
  type StoreBrowsePreferences,
} from './cache';
import {
  applyUpdateCheck,
  savePreviousUpdates,
  verifySavedUpdates,
  type StoreUpdateResults,
} from './updatesState';

type StoreOperation = 'save' | 'install' | 'open' | 'restart' | 'discard';
interface DetailSelection extends StoreReleaseChoice {
  store_url: string;
  applet_id: string;
  installed_source?: boolean;
}

export function useStore(
  onLaunch: (session: AppletSession) => void,
  onBusyChange: (busy: boolean) => void,
) {
  const [config, setConfig] = useState<StoreConfig>();
  const [browse, setBrowse] = useState(DEFAULT_BROWSE);
  const [catalog, setCatalog] = useState<StoreCatalog>();
  const [updates, setUpdates] = useState<StoreUpdateResults>();
  const [updates_query, setUpdatesQuery] = useState('');
  const [detail, setDetail] = useState<StoreDetail>();
  const [pending_download, setPendingDownload] = useState<StorePendingDownload>();
  const [local_installation, setLocalInstallation] = useState<AppletInstallResult>();
  const [detail_selection, setDetailSelection] = useState<DetailSelection>();
  const [address_open, setAddressOpen] = useState(false);
  const [initial_loading, setInitialLoading] = useState(true);
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
  const [progress, setProgress] = useState<InstallProgress>();
  const scope = useRef({
    alive: false,
    config: 0,
    catalog: 0,
    updates: 0,
    detail: 0,
    pending: 0,
    operation: '',
  });
  const current_url = useRef<string | null>(null);
  const cache = useRef<StoreBrowseCache | undefined>(undefined);
  const selection = useRef<DetailSelection | undefined>(undefined);
  const fresh = useRef(false);

  const selectDetail = (next?: DetailSelection) => {
    selection.current = next;
    setDetailSelection(next);
  };

  const markFresh = (value: boolean) => {
    fresh.current = value;
    setDetailFresh(value);
  };

  const adoptStore = (store_url: string | null) => {
    if (current_url.current === store_url) return;
    current_url.current = store_url;
    scope.current.catalog++;
    const next = store_url ? readStoreCache(store_url) : undefined;
    cache.current = next;
    setBrowse((current) => {
      const preferences = next?.preferences ?? DEFAULT_BROWSE;
      return current.updates_only ? { ...preferences, updates_only: true } : preferences;
    });
    setCatalog(next && cachedCatalog(next));
    setCatalogError('');
    setCatalogLoading(false);
    if (selection.current?.installed_source) return;
    scope.current.detail++;
    selectDetail();
    setDetail(undefined);
    setLocalInstallation(undefined);
    setDetailError('');
    setDetailLoading(false);
    markFresh(false);
  };

  const updateBrowse = (patch: Partial<StoreBrowsePreferences>) => {
    const next = {
      ...browse,
      ...patch,
      ...(patch.query !== undefined ? { query: patch.query.slice(0, 200) } : {}),
    };
    if (patch.updates_only && !browse.updates_only) setUpdates(savePreviousUpdates);
    setBrowse(next);
    if (cache.current) {
      cache.current.preferences = next;
      writeStorePreferences(cache.current.store_url, next);
    }
  };

  const isCurrentCatalog = (store_url: string, request: number) =>
    scope.current.alive && scope.current.catalog === request && current_url.current === store_url;

  const loadSavedInstallations = async (store_url: string, request: number) => {
    if (catalog?.store_url !== store_url && !cache.current?.catalog) return;
    try {
      const report = await listApplets();
      if (isCurrentCatalog(store_url, request)) {
        setCatalog((current) =>
          current?.store_url === store_url ? reviewSavedCatalog(current, report) : current,
        );
      }
    } catch {
      // Saved listings remain available even if the native inventory fails.
    }
  };

  const clearCatalogReview = () => setCatalog((current) => current && reviewSavedCatalog(current));

  const loadCatalog = async (store_url: string) => {
    const request = ++scope.current.catalog;
    setCatalogLoading(true);
    setCatalogError('');
    clearCatalogReview();
    try {
      const result = await getStoreCatalog(store_url);
      if (isCurrentCatalog(store_url, request)) {
        setCatalog(result);
        if (cache.current?.store_url === store_url) {
          cacheStoreCatalog(cache.current, result);
          writeStoreCache(cache.current);
        }
      }
    } catch (error) {
      if (isCurrentCatalog(store_url, request)) {
        setCatalogError(message(error));
        await loadSavedInstallations(store_url, request);
      }
    } finally {
      if (isCurrentCatalog(store_url, request)) setCatalogLoading(false);
    }
  };

  const loadUpdates = async () => {
    const request = ++scope.current.updates;
    const isCurrent = () => scope.current.alive && scope.current.updates === request;
    setUpdatesLoading(true);
    setUpdatesError('');
    setUpdates(savePreviousUpdates);
    try {
      const result = await getStoreUpdates();
      if (isCurrent()) setUpdates((previous) => applyUpdateCheck(result, previous));
    } catch (error) {
      if (!isCurrent()) return;
      setUpdatesError(message(error));
      if (updates?.sources.some((source) => source.updates.length || source.saved_updates.length)) {
        const report = await listApplets().catch(() => undefined);
        if (report && isCurrent()) setUpdates((previous) => verifySavedUpdates(previous, report));
      }
    } finally {
      if (isCurrent()) setUpdatesLoading(false);
    }
  };

  const loadPendingDownload = async () => {
    const request = ++scope.current.pending;
    try {
      const result = await getStorePendingDownload();
      if (scope.current.alive && scope.current.pending === request) {
        setPendingDownload(result ?? undefined);
      }
    } catch {
      // A local checkpoint error does not prevent browsing the Store.
    }
  };

  const loadDetail = async (
    { store_url, applet_id, installed_source }: DetailSelection,
    just_installed?: AppletInstallResult,
  ) => {
    const request = ++scope.current.detail;
    const isCurrent = () =>
      scope.current.alive &&
      scope.current.detail === request &&
      selection.current?.store_url === store_url &&
      selection.current.applet_id === applet_id &&
      selection.current.installed_source === installed_source;
    markFresh(false);
    setDetailLoading(true);
    setDetailError('');
    setLocalInstallation(just_installed);
    void listApplets().then(
      (report) => {
        if (isCurrent()) {
          const applet = report.applets.find((applet) => applet.applet_id === applet_id);
          setLocalInstallation(
            applet && { applet, restart_required: report.restart_required.includes(applet_id) },
          );
        }
      },
      () => {},
    );
    try {
      const result = await getStoreDetail(store_url, applet_id, installed_source);
      const requested = selection.current;
      if (isCurrent() && requested) {
        setDetail(result);
        markFresh(true);
        if (
          !installed_source &&
          !requested.pending_download &&
          cache.current?.store_url === store_url
        ) {
          const remembered = requested.version ?? requested.remembered_version;
          const version = requested.channel
            ? reviewStoreRelease(result, requested).release?.version
            : result.releases.find((release) => release.version === remembered)?.version;
          cacheStoreDetail(cache.current, result, version ?? '');
          writeStoreCache(cache.current);
        }
      }
    } catch (error) {
      if (isCurrent()) setDetailError(message(error));
    } finally {
      if (isCurrent()) setDetailLoading(false);
    }
  };

  const loadConfig = async () => {
    const request = ++scope.current.config;
    if (!config) setInitialLoading(true);
    setConfigError('');
    try {
      const result = await getStoreConfig();
      if (!scope.current.alive || scope.current.config !== request) return;
      adoptStore(result.store_url);
      setConfig(result);
      if (result.store_url) void loadCatalog(result.store_url);
      if (selection.current) void loadDetail(selection.current);
    } catch (error) {
      if (scope.current.alive && scope.current.config === request) {
        if (!config) setConfigError(message(error));
        if (selection.current) void loadDetail(selection.current);
        if (config?.store_url) {
          setCatalogError(message(error));
          setCatalogLoading(false);
          void loadSavedInstallations(config.store_url, ++scope.current.catalog);
        }
      }
    } finally {
      if (scope.current.alive && scope.current.config === request) setInitialLoading(false);
    }
  };

  useLayoutEffect(() => {
    scope.current.alive = true;
    markFresh(false);
    clearCatalogReview();
    setUpdates(savePreviousUpdates);
    if (selection.current) {
      setLocalInstallation(undefined);
      setDetailLoading(true);
      setDetailError('');
    }
    return () => {
      scope.current.alive = false;
      fresh.current = false;
      scope.current.config++;
      scope.current.catalog++;
      scope.current.updates++;
      scope.current.detail++;
      scope.current.pending++;
    };
  }, []);
  useEffect(() => {
    void loadConfig();
    void loadPendingDownload();
  }, []);
  useEffect(() => {
    if (browse.updates_only) void loadUpdates();
  }, [browse.updates_only]);

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
        onBusyChange(false);
        setOperation(undefined);
      }
    }
  };

  const closeDetail = () => {
    scope.current.detail++;
    selectDetail();
    markFresh(false);
    setDetail(undefined);
    setLocalInstallation(undefined);
    setDetailError('');
    setDetailLoading(false);
    setActionError('');
    setStatus('');
  };

  const reviewDownload = () => {
    if (scope.current.operation || !pending_download) return;
    setActionError('');
    setStatus('');
    const requested = {
      store_url: pending_download.origin,
      applet_id: pending_download.applet_id,
      installed_source: pending_download.origin !== current_url.current,
      pending_download,
    };
    selectDetail(requested);
    setDetail(undefined);
    void loadDetail(requested);
  };

  const discardDownload = () => {
    const pending = selection.current?.pending_download;
    if (!pending) return;
    void run('discard', async () => {
      const discarded = await discardStoreDownload(pending);
      if (discarded && scope.current.alive) setPendingDownload(undefined);
      await loadPendingDownload();
      if (!scope.current.alive) return;
      closeDetail();
      setStatus(discarded ? 'Download discarded.' : 'This paused download is no longer available.');
    });
  };

  const saveAddress = (value: string) =>
    void run('save', async () => {
      scope.current.config++;
      const result = await setStoreUrl(value.trim());
      if (!scope.current.alive) return;
      adoptStore(result.store_url);
      setConfig(result);
      setAddressOpen(false);
      if (result.store_url) void loadCatalog(result.store_url);
    });

  const install = (release: StoreReleaseOption, confirmed_channel_switch = false) => {
    const requested = selection.current;
    if (!fresh.current || !detail?.releases.includes(release) || !requested) return;
    const resume = !!requested.pending_download;
    if (
      resume &&
      reviewStoreRelease(detail, requested, requested.installed_source).release !== release
    )
      return;
    const switching = release.action === 'switch_source';
    const switching_channel = release.action === 'switch_channel';
    if (requested.installed_source) {
      const same_channel_update =
        release.action === 'update' && storeReleaseChannel(release) === detail.installed_channel;
      if (!same_channel_update && !(resume && switching_channel)) return;
    } else if (requested.store_url !== current_url.current) return;
    if (switching && !detail.switch_snapshot) return;
    if (switching_channel && (!confirmed_channel_switch || !detail.channel_snapshot)) return;
    if (!switching && !switching_channel && !['install', 'update'].includes(release.action)) return;
    const snapshot = detail;
    void run('install', async (operation_id) => {
      setProgress(undefined);
      const stop = await onInstallProgress((event) => {
        if (
          scope.current.alive &&
          scope.current.operation === operation_id &&
          event.operation_id === operation_id
        )
          setProgress(event);
      }).catch(() => () => {});
      try {
        if (!scope.current.alive) return;
        const installed = await installStoreRelease({
          store_url: snapshot.store_url,
          applet_id: snapshot.applet.applet_id,
          release,
          expected_installed_sha256: snapshot.installed?.sha256 ?? null,
          ...(switching ? { switch_snapshot: snapshot.switch_snapshot } : {}),
          ...(switching_channel ? { channel_snapshot: snapshot.channel_snapshot } : {}),
          ...(requested.installed_source ? { installed_source: true } : {}),
          ...(resume ? { resume_download: true } : {}),
          operation_id,
        });
        if (!scope.current.alive) return;
        if (installed.status === 'paused') {
          if (resume && installed.received_bytes === 0) {
            closeDetail();
            setStatus(
              'The download stopped and no saved progress remains. Open the applet in the Store to start again.',
            );
            return;
          }
          setDetail(
            (current) =>
              current && {
                ...current,
                releases: current.releases.map((option) => ({
                  ...option,
                  downloaded_bytes:
                    option.version === release.version &&
                    option.sha256 === release.sha256 &&
                    installed.received_bytes > 0
                      ? installed.received_bytes
                      : undefined,
                })),
              },
          );
          setStatus(
            installed.received_bytes > 0
              ? 'Download paused. Continue when you’re ready.'
              : 'Download interrupted. Try again when you’re ready.',
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
        const next_selection = {
          ...requested,
          pending_download: undefined,
          version: requested.pending_download?.version ?? selection.current?.version,
          channel: requested.installed_source ? undefined : storeReleaseChannel(release),
        };
        selectDetail(next_selection);
        void loadDetail(next_selection, installed);
        if (snapshot.store_url === current_url.current) void loadCatalog(snapshot.store_url);
        if (requested.installed_source || updates) void loadUpdates();
      } catch (error) {
        if (resume && scope.current.alive) void loadDetail(requested);
        throw error;
      } finally {
        stop();
        if (scope.current.alive) await loadPendingDownload();
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
    if (!displayed_detail || !requested || requested.installed_source || requested.pending_download)
      return;
    const next = { ...requested, ...choice, remembered_version: undefined };
    selectDetail(next);
    const record = cache.current?.details.find(
      (entry) => entry.applet.applet_id === requested.applet_id,
    );
    if (record && cache.current?.store_url === requested.store_url) {
      record.selected_version = reviewStoreRelease(displayed_detail, choice).release?.version ?? '';
      writeStoreCache(cache.current);
    }
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
    pending_download,
    reviewing_download: detail_selection?.pending_download,
    reviewDownload,
    discardDownload,
    installed,
    restart_required,
    selected_id: detail_selection?.applet_id,
    review:
      displayed_detail &&
      reviewStoreRelease(displayed_detail, detail_selection, detail_selection?.installed_source),
    setChannel: (channel: StoreChannel) => chooseRelease({ channel, version: undefined }),
    setSelectedVersion: (version: string) => {
      const release = displayed_detail?.releases.find((option) => option.version === version);
      if (release) chooseRelease({ version, channel: storeReleaseChannel(release) });
    },
    address_open,
    setAddressOpen,
    initial_loading,
    catalog_loading,
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
    progress,
    loadConfig,
    saveAddress,
    closeDetail: () => {
      if (!scope.current.operation) closeDetail();
    },
    install,
    installed_source: detail_selection?.installed_source === true,
    openDetail: (applet_id: string) => {
      if (!current_url.current || scope.current.operation) return;
      setStatus('');
      setActionError('');
      const fallback = cache.current && cachedDetail(cache.current, applet_id);
      const remembered_version = cache.current?.details.find(
        (entry) => entry.applet.applet_id === applet_id,
      )?.selected_version;
      const requested = { store_url: current_url.current, applet_id, remembered_version };
      selectDetail(requested);
      setDetail(fallback);
      void loadDetail(requested);
    },
    openUpdate: (source: StoreSource) => {
      if (scope.current.operation) return;
      setStatus('');
      setActionError('');
      const requested = {
        store_url: source.store_url,
        applet_id: source.applet_id,
        installed_source: true,
      };
      selectDetail(requested);
      setDetail(undefined);
      void loadDetail(requested);
    },
    refreshCatalog: () => {
      if (scope.current.operation) return;
      void loadPendingDownload();
      if (current_url.current) void loadCatalog(current_url.current);
      else if (!config) void loadConfig();
    },
    refreshUpdates: () => {
      if (scope.current.operation) return;
      void loadPendingDownload();
      void loadUpdates();
    },
    retryDetail: () => {
      if (selection.current && !scope.current.operation) void loadDetail(selection.current);
    },
    openApplet: () => {
      if (installed && selection.current?.applet_id === installed.applet_id) {
        openInstalled(installed.applet_id, restart_required);
      }
    },
  };
}
