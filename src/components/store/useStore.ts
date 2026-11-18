import { useEffect, useRef, useState } from 'react';
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

type StoreOperation = 'save' | 'install' | 'open' | 'restart';

export function useStore(
  onLaunch: (session: AppletSession) => void,
  onBusyChange: (busy: boolean) => void,
) {
  const [config, setConfig] = useState<StoreConfig>();
  const [catalog, setCatalog] = useState<StoreCatalog>();
  const [detail, setDetail] = useState<StoreDetail>();
  const [selected_id, setSelectedId] = useState<string>();
  const [selected_version, setSelectedVersion] = useState('');
  const [address_open, setAddressOpen] = useState(false);
  const [initial_loading, setInitialLoading] = useState(true);
  const [catalog_loading, setCatalogLoading] = useState(false);
  const [detail_loading, setDetailLoading] = useState(false);
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
  const detail_channel = useRef<StoreChannel | undefined>(undefined);

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
      )
        setCatalog(result);
    } catch (error) {
      if (scope.current.alive && scope.current.catalog === request) setCatalogError(message(error));
    } finally {
      if (scope.current.alive && scope.current.catalog === request) setCatalogLoading(false);
    }
  };

  const loadDetail = async (
    store_url: string,
    applet_id: string,
    version?: string,
    channel?: StoreChannel,
  ) => {
    const request = ++scope.current.detail;
    setSelectedId(applet_id);
    setDetail(undefined);
    setDetailLoading(true);
    setDetailError('');
    try {
      const result = await getStoreDetail(store_url, applet_id);
      if (
        scope.current.alive &&
        scope.current.detail === request &&
        current_url.current === store_url
      ) {
        setDetail(result);
        setSelectedVersion(
          version && result.releases.some((release) => release.version === version)
            ? version
            : recommendedStoreRelease(result, channel).version,
        );
      }
    } catch (error) {
      if (scope.current.alive && scope.current.detail === request) setDetailError(message(error));
    } finally {
      if (scope.current.alive && scope.current.detail === request) setDetailLoading(false);
    }
  };

  const loadConfig = async () => {
    const request = ++scope.current.config;
    setInitialLoading(true);
    setConfigError('');
    try {
      const result = await getStoreConfig();
      if (!scope.current.alive || scope.current.config !== request) return;
      current_url.current = result.store_url;
      setConfig(result);
      if (result.store_url) void loadCatalog(result.store_url);
    } catch (error) {
      if (scope.current.alive && scope.current.config === request) setConfigError(message(error));
    } finally {
      if (scope.current.alive && scope.current.config === request) setInitialLoading(false);
    }
  };

  useEffect(() => {
    scope.current.alive = true;
    void loadConfig();
    return () => {
      scope.current.alive = false;
      scope.current.config++;
      scope.current.catalog++;
      scope.current.detail++;
      onBusyChange(false);
    };
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
      if (scope.current.operation === id) scope.current.operation = '';
      onBusyChange(false);
      if (scope.current.alive) setOperation(undefined);
    }
  };

  const closeDetail = () => {
    if (scope.current.operation) return;
    scope.current.detail++;
    setSelectedId(undefined);
    setDetail(undefined);
    setDetailError('');
    setDetailLoading(false);
    setActionError('');
    setRestartAfterInstall(undefined);
  };

  const saveAddress = (value: string) =>
    void run('save', async () => {
      const result = await setStoreUrl(value.trim());
      if (!scope.current.alive) return;
      current_url.current = result.store_url;
      scope.current.catalog++;
      scope.current.detail++;
      setConfig(result);
      setCatalog(undefined);
      setCatalogError('');
      setCatalogLoading(false);
      setSelectedId(undefined);
      setDetail(undefined);
      setAddressOpen(false);
      if (result.store_url) void loadCatalog(result.store_url);
    });

  const install = (release: StoreReleaseOption, confirmed_channel_switch = false) => {
    if (!detail || !current_url.current) return;
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
        void loadDetail(snapshot.store_url, snapshot.applet.applet_id, release.version);
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
    catalog,
    detail,
    selected_id,
    selected_version,
    setSelectedVersion,
    address_open,
    setAddressOpen,
    initial_loading,
    catalog_loading,
    detail_loading,
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
    openDetail: (applet_id: string, channel?: StoreChannel) => {
      setStatus('');
      setActionError('');
      setRestartAfterInstall(undefined);
      if (current_url.current && !scope.current.operation) {
        detail_channel.current = channel;
        void loadDetail(current_url.current, applet_id, undefined, channel);
      }
    },
    refreshCatalog: () => {
      if (current_url.current && !scope.current.operation) void loadCatalog(current_url.current);
    },
    retryDetail: () => {
      if (current_url.current && selected_id && !scope.current.operation)
        void loadDetail(current_url.current, selected_id, undefined, detail_channel.current);
    },
    openApplet: () => {
      if (detail?.installed) openInstalled(detail.applet.applet_id, detail.restart_required);
      else if (restart_after_install) openInstalled(restart_after_install, true);
    },
  };
}
