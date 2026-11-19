import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  getStoreCatalog,
  getStoreConfig,
  getStoreDetail,
  installStoreRelease,
  launchApplet,
  message,
  onInstallProgress,
  restartApplet,
  setStoreUrl,
} from '../../lib/tauri';
import type {
  AppletSession,
  InstallProgress,
  StoreCatalog,
  StoreChannel,
  StoreConfig,
  StoreDetail,
  StoreReleaseOption,
} from '../../lib/types';
import {
  recommendedStoreRelease,
  storeChannelName,
  storeReleaseChannel,
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

type StoreOperation = 'save' | 'install' | 'open' | 'restart';
interface DetailSelection {
  applet_id: string;
  version?: string;
  channel?: StoreChannel;
}

export function useStore(
  onLaunch: (session: AppletSession) => void,
  onBusyChange: (busy: boolean) => void,
) {
  const [config, setConfig] = useState<StoreConfig>();
  const [browse, setBrowse] = useState(DEFAULT_BROWSE);
  const [catalog, setCatalog] = useState<StoreCatalog>();
  const [detail, setDetail] = useState<StoreDetail>();
  const [selected_id, setSelectedId] = useState<string>();
  const [selected_version, setSelectedVersion] = useState('');
  const [address_open, setAddressOpen] = useState(false);
  const [initial_loading, setInitialLoading] = useState(true);
  const [catalog_loading, setCatalogLoading] = useState(false);
  const [detail_loading, setDetailLoading] = useState(false);
  const [detail_fresh, setDetailFresh] = useState(false);
  const [config_error, setConfigError] = useState('');
  const [catalog_error, setCatalogError] = useState('');
  const [detail_error, setDetailError] = useState('');
  const [status, setStatus] = useState('');
  const [restart_after_install, setRestartAfterInstall] = useState<string>();
  const [action_error, setActionError] = useState('');
  const [operation, setOperation] = useState<StoreOperation>();
  const [progress, setProgress] = useState<InstallProgress>();
  const scope = useRef({ alive: false, config: 0, catalog: 0, detail: 0, operation: '' });
  const current_url = useRef<string | null>(null);
  const cache = useRef<StoreBrowseCache | undefined>(undefined);
  const selection = useRef<DetailSelection | undefined>(undefined);
  const fresh = useRef(false);

  const markFresh = (value: boolean) => {
    fresh.current = value;
    setDetailFresh(value);
  };

  const adoptStore = (store_url: string | null) => {
    if (current_url.current === store_url) return;
    current_url.current = store_url;
    scope.current.catalog++;
    scope.current.detail++;
    const next = store_url ? readStoreCache(store_url) : undefined;
    cache.current = next;
    setBrowse(next?.preferences ?? DEFAULT_BROWSE);
    setCatalog(next && cachedCatalog(next));
    setCatalogError('');
    setCatalogLoading(false);
    selection.current = undefined;
    setSelectedId(undefined);
    setSelectedVersion('');
    setDetail(undefined);
    setDetailError('');
    setDetailLoading(false);
    setRestartAfterInstall(undefined);
    markFresh(false);
  };

  const updateBrowse = (patch: Partial<StoreBrowsePreferences>) => {
    if (!cache.current) return;
    const next = {
      ...cache.current.preferences,
      ...patch,
      ...(patch.query !== undefined ? { query: patch.query.slice(0, 200) } : {}),
    };
    cache.current.preferences = next;
    setBrowse(next);
    writeStorePreferences(cache.current.store_url, next);
  };

  const loadCatalog = async (store_url: string) => {
    const request = ++scope.current.catalog;
    setCatalogLoading(true);
    setCatalogError('');
    try {
      const result = await getStoreCatalog(store_url);
      if (
        scope.current.alive &&
        scope.current.catalog === request &&
        current_url.current === store_url
      ) {
        setCatalog(result);
        if (cache.current?.store_url === store_url) {
          cacheStoreCatalog(cache.current, result);
          writeStoreCache(cache.current);
        }
      }
    } catch (error) {
      if (scope.current.alive && scope.current.catalog === request) setCatalogError(message(error));
    } finally {
      if (scope.current.alive && scope.current.catalog === request) setCatalogLoading(false);
    }
  };

  const loadDetail = async (store_url: string, applet_id: string) => {
    const request = ++scope.current.detail;
    markFresh(false);
    setDetailLoading(true);
    setDetailError('');
    try {
      const result = await getStoreDetail(store_url, applet_id);
      if (
        scope.current.alive &&
        scope.current.detail === request &&
        current_url.current === store_url &&
        selection.current?.applet_id === applet_id
      ) {
        const requested = selection.current;
        const explicit = result.releases.find((option) => option.version === requested.version);
        const release = explicit ?? recommendedStoreRelease(result, requested.channel);
        setSelectedVersion(release.version);
        selection.current = {
          applet_id,
          version: explicit?.version,
          channel: requested.channel,
        };
        setDetail(result);
        markFresh(true);
        if (cache.current?.store_url === store_url) {
          const remembered_version = cache.current.details.find(
            (entry) => entry.applet.applet_id === applet_id,
          )?.selected_version;
          cacheStoreDetail(cache.current, result, explicit?.version ?? remembered_version ?? '');
          writeStoreCache(cache.current);
        }
      }
    } catch (error) {
      if (scope.current.alive && scope.current.detail === request) setDetailError(message(error));
    } finally {
      if (scope.current.alive && scope.current.detail === request) setDetailLoading(false);
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
      if (result.store_url) {
        void loadCatalog(result.store_url);
        if (selection.current) void loadDetail(result.store_url, selection.current.applet_id);
      }
    } catch (error) {
      if (scope.current.alive && scope.current.config === request) {
        if (selection.current) {
          setDetailError(message(error));
          setDetailLoading(false);
        } else if (config?.store_url) {
          setCatalogError(message(error));
          setCatalogLoading(false);
        } else setConfigError(message(error));
      }
    } finally {
      if (scope.current.alive && scope.current.config === request) setInitialLoading(false);
    }
  };

  useLayoutEffect(() => {
    scope.current.alive = true;
    markFresh(false);
    if (selection.current) {
      setDetailLoading(true);
      setDetailError('');
    }
    return () => {
      scope.current.alive = false;
      fresh.current = false;
      scope.current.config++;
      scope.current.catalog++;
      scope.current.detail++;
    };
  }, []);
  useEffect(() => {
    void loadConfig();
  }, []);

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
    if (scope.current.operation) return;
    scope.current.detail++;
    selection.current = undefined;
    markFresh(false);
    setSelectedId(undefined);
    setDetail(undefined);
    setDetailError('');
    setDetailLoading(false);
    setActionError('');
    setRestartAfterInstall(undefined);
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
    if (!fresh.current || !detail?.releases.includes(release) || !current_url.current) return;
    const switching = release.action === 'switch_source';
    const switching_channel = release.action === 'switch_channel';
    if (switching && !detail.switch_snapshot) return;
    if (switching_channel && (!confirmed_channel_switch || !detail.channel_snapshot)) return;
    if (!switching && !switching_channel && !['install', 'update'].includes(release.action)) return;
    const snapshot = detail;
    void run('install', async (operation_id) => {
      setRestartAfterInstall(undefined);
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
          operation_id,
        });
        if (!scope.current.alive) return;
        if (installed.status === 'paused') {
          setDetail(
            (current) =>
              current && {
                ...current,
                releases: current.releases.map((option) => ({
                  ...option,
                  downloaded_bytes:
                    option.sha256 === release.sha256 ? installed.received_bytes : undefined,
                })),
              },
          );
          setStatus('Download paused. Continue when you’re ready.');
          return;
        }
        setStatus(
          switching
            ? 'Switched to this store.'
            : switching_channel
              ? `Switched to ${storeChannelName(storeReleaseChannel(release))}.`
              : 'Installed.',
        );
        if (installed.restart_required) setRestartAfterInstall(installed.applet.applet_id);
        selection.current = {
          applet_id: snapshot.applet.applet_id,
          version: selection.current?.version,
          channel: storeReleaseChannel(release),
        };
        void loadDetail(snapshot.store_url, snapshot.applet.applet_id);
        void loadCatalog(snapshot.store_url);
      } finally {
        stop();
      }
    });
  };

  const openInstalled = (applet_id: string, restart: boolean) =>
    void run(restart ? 'restart' : 'open', async () => {
      const launch = restart ? restartApplet : launchApplet;
      const session = await launch(applet_id);
      if (scope.current.alive) onLaunch(session);
    });

  return {
    config,
    browse,
    updateBrowse,
    catalog,
    detail,
    selected_id,
    selected_version,
    setSelectedVersion: (version: string) => {
      if (!detail || !selection.current) return;
      const release = detail.releases.find((option) => option.version === version);
      if (!release) return;
      selection.current = { ...selection.current, version, channel: storeReleaseChannel(release) };
      setSelectedVersion(version);
      const record = cache.current?.details.find(
        (entry) => entry.applet.applet_id === detail.applet.applet_id,
      );
      if (record && cache.current) {
        record.selected_version = version;
        writeStoreCache(cache.current);
      }
    },
    address_open,
    setAddressOpen,
    initial_loading,
    catalog_loading,
    detail_loading,
    detail_fresh,
    config_error,
    catalog_error,
    detail_error,
    status,
    restart_after_install,
    action_error,
    operation,
    progress,
    loadConfig,
    saveAddress,
    closeDetail,
    install,
    openDetail: (applet_id: string, channel?: StoreChannel, use_remembered_version = true) => {
      if (!current_url.current || scope.current.operation) return;
      setStatus('');
      setActionError('');
      setRestartAfterInstall(undefined);
      const fallback = cache.current && cachedDetail(cache.current, applet_id);
      const remembered = cache.current?.details.find(
        (entry) => entry.applet.applet_id === applet_id,
      )?.selected_version;
      const previous = fallback?.releases.find(
        (release) =>
          use_remembered_version &&
          release.version === remembered &&
          (!channel || storeReleaseChannel(release) === channel),
      );
      const preferred = previous ?? (fallback && recommendedStoreRelease(fallback, channel));
      // Do not carry a catalog-only fallback across a network response: an
      // installed applet may have an authoritative channel on the device.
      selection.current = { applet_id, version: previous?.version, channel };
      setSelectedId(applet_id);
      setDetail(fallback);
      setSelectedVersion(preferred?.version ?? '');
      void loadDetail(current_url.current, applet_id);
    },
    refreshCatalog: () => {
      if (current_url.current && !scope.current.operation) void loadCatalog(current_url.current);
    },
    retryDetail: () => {
      if (current_url.current && selected_id && !scope.current.operation)
        void loadDetail(current_url.current, selected_id);
    },
    openApplet: () => {
      if (fresh.current && detail?.installed) {
        openInstalled(detail.applet.applet_id, detail.restart_required);
      } else if (restart_after_install) openInstalled(restart_after_install, true);
    },
  };
}
