import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  getStoreDetail,
  installStoreRelease,
  message,
  onAppletInstallationChanged,
  onInstallProgress,
} from '../lib/tauri';
import {
  newStoreAccess,
  recommendedStoreRelease,
  storeChannelName,
  storeReleaseChannel,
  storeSize,
} from '../lib/storeState';
import type {
  AppletInstallResult,
  AppletInstallation,
  InstallProgress,
  StoreChannel,
  StoreDetail,
  StoreReleaseOption,
  StoreSource,
} from '../lib/types';
import { OperationScope, type Operation } from './providers/state';

interface Review {
  detail: StoreDetail;
  release: StoreReleaseOption;
}

type Phase = 'loading' | 'checking' | 'installing';
const installation_changed = 'The installed applet changed. Check its releases again.';

function sameStoreSource(first: StoreSource | null, second: StoreSource | null) {
  return (
    first?.applet_id === second?.applet_id &&
    first?.store_url === second?.store_url &&
    first?.channel === second?.channel
  );
}

function sameInstallation(first: AppletInstallation | undefined, second: AppletInstallation) {
  return (
    first?.installed_version === second.installed_version &&
    first.installed_sha256 === second.installed_sha256 &&
    sameStoreSource(first.store_source, second.store_source)
  );
}

export function AppletChannel({
  applet_id,
  loadInstallation,
  initial_installation,
  disabled = false,
  onInstalling,
  onInstalled,
  onRestart,
}: {
  applet_id: string;
  loadInstallation: () => Promise<AppletInstallation>;
  initial_installation?: AppletInstallation;
  disabled?: boolean;
  onInstalling: (installing: boolean) => void;
  onInstalled?: (result: AppletInstallResult) => void;
  onRestart: () => void;
}) {
  const [installation, setInstallation] = useState(initial_installation);
  const [review, setReview] = useState<Review>();
  const [phase, setPhase] = useState<Phase>();
  const [refresh_requested, setRefreshRequested] = useState(false);
  const [progress, setProgress] = useState<InstallProgress>();
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());
  const last_initial = useRef(initial_installation);
  const current_installation = useRef(initial_installation);
  const load_installation = useRef(loadInstallation);
  load_installation.current = loadInstallation;

  const updateInstallation = (next: AppletInstallation) => {
    current_installation.current = next;
    setInstallation(next);
  };

  useLayoutEffect(() => {
    if (!initial_installation || phase === 'installing') return;
    const previous = last_initial.current;
    last_initial.current = initial_installation;
    if (sameInstallation(previous, initial_installation)) {
      if (
        sameInstallation(current_installation.current, initial_installation) &&
        previous?.restart_required !== initial_installation.restart_required
      ) {
        updateInstallation(initial_installation);
      }
      return;
    }
    if (!sameInstallation(current_installation.current, initial_installation)) {
      scope.current.cancel();
      setPhase(undefined);
      setReview(undefined);
      setNotice('');
      setError(phase === 'checking' || review ? installation_changed : '');
    }
    updateInstallation(initial_installation);
  }, [initial_installation, phase]);

  const run = async (next: Phase, work: (operation: Operation) => Promise<void>) => {
    const operation = scope.current.start();
    if (!operation) return;
    setPhase(next);
    setError('');
    setNotice('');
    if (next === 'installing') onInstalling(true);
    try {
      await work(operation);
    } catch (error) {
      if (operation.isCurrent()) setError(message(error));
    } finally {
      if (operation.isCurrent()) {
        operation.finish();
        setPhase(undefined);
      }
      if (next === 'installing') onInstalling(false);
    }
  };

  const reload = async (operation: Operation) => {
    const installed = await load_installation.current();
    if (installed.store_source && installed.store_source.applet_id !== applet_id) {
      throw new Error('Could not verify this applet’s installation.');
    }
    if (operation.isCurrent()) updateInstallation(installed);
    return installed;
  };
  const load = () =>
    void run('loading', async (operation) => {
      const previous = current_installation.current;
      try {
        const next = await reload(operation);
        if (operation.isCurrent() && !sameInstallation(previous, next)) {
          setReview(undefined);
          if (review) setError(installation_changed);
        }
      } catch (error) {
        if (operation.isCurrent()) {
          current_installation.current = undefined;
          setInstallation(undefined);
          setReview(undefined);
        }
        throw error;
      }
    });
  useEffect(() => {
    if (last_initial.current) return () => scope.current.cancel();
    let open = true;
    const subscription = onAppletInstallationChanged((changed_id) => {
      if (open && changed_id === applet_id) setRefreshRequested(true);
    }).catch(() => undefined);
    void subscription.then(() => {
      if (open) setRefreshRequested(true);
    });
    return () => {
      open = false;
      scope.current.cancel();
      void subscription.then((unlisten) => unlisten?.());
    };
  }, [applet_id]);
  useEffect(() => {
    if (!refresh_requested || phase) return;
    setRefreshRequested(false);
    load();
  }, [refresh_requested, phase]);

  const source = installation?.store_source;
  const controls_disabled = disabled || !!phase || refresh_requested;
  if (!installation) {
    if (!error) return <p role="status">Checking installation…</p>;
    return (
      <div className="applet-channel">
        <p role="alert">{error}</p>
        <button disabled={controls_disabled} onClick={load}>
          Retry installation check
        </button>
      </div>
    );
  }
  const restart = installation.restart_required && !review && (
    <>
      <p role="status">Version {installation.installed_version} is installed. Restart to use it.</p>
      <button className="primary" disabled={controls_disabled} onClick={onRestart}>
        Restart applet
      </button>
    </>
  );
  if (!source) {
    return restart || error ? (
      <div className="applet-channel">
        {error && <p role="alert">{error}</p>}
        {restart}
      </div>
    ) : null;
  }

  const checkRelease = (channel: StoreChannel) =>
    void run('checking', async (operation) => {
      setReview(undefined);
      let detail: StoreDetail;
      try {
        detail = await getStoreDetail(source.store_url, applet_id, true);
      } catch (error) {
        if (!operation.isCurrent()) return;
        const installed = await reload(operation).catch(() => undefined);
        if (installed && !sameInstallation(installation, installed)) {
          throw new Error(installation_changed);
        }
        throw error;
      }
      if (!operation.isCurrent()) return;
      const installed = detail.installed;
      const snapshot = detail.channel_snapshot;
      if (
        detail.applet.applet_id !== applet_id ||
        detail.store_url !== source.store_url ||
        detail.installed_channel !== source.channel ||
        installed?.applet_id !== applet_id ||
        installed.version !== installation.installed_version ||
        (installation.installed_sha256 && installed.sha256 !== installation.installed_sha256) ||
        installed.source?.kind !== 'store' ||
        !sameStoreSource(installed.source, source) ||
        !snapshot ||
        snapshot.removed ||
        snapshot.sha256 !== installed.sha256 ||
        snapshot.source?.kind !== 'store' ||
        !sameStoreSource(snapshot.source, source)
      ) {
        await reload(operation).catch(() => undefined);
        throw new Error(installation_changed);
      }
      updateInstallation({
        store_source: source,
        installed_version: installed.version,
        installed_sha256: installed.sha256,
        restart_required: detail.restart_required,
      });
      const recommended = recommendedStoreRelease(detail, channel);
      const release = detail.releases.find(
        (item) => item.version === recommended.version && storeReleaseChannel(item) === channel,
      );
      if (!release) throw new Error(`No ${storeChannelName(channel)} release is available yet.`);
      const same_channel = channel === source.channel;
      if (same_channel && (release.action === 'installed' || release.action === 'older')) {
        setNotice('You’re up to date.');
        return;
      }
      if (release.action !== (same_channel ? 'update' : 'switch_channel'))
        throw new Error(release.action_reason ?? 'This release cannot be installed.');
      setReview({ detail, release });
    });

  const install = (release_review: Review) =>
    void run('installing', async (operation) => {
      const { detail, release } = release_review;
      const operation_id = crypto.randomUUID();
      setProgress(undefined);
      const stop = await onInstallProgress((event) => {
        if (operation.isCurrent() && event.operation_id === operation_id) setProgress(event);
      }).catch(() => () => {});
      try {
        if (!operation.isCurrent()) return;
        const result = await installStoreRelease({
          store_url: detail.store_url,
          applet_id: detail.applet.applet_id,
          release,
          ...(release.action === 'switch_channel'
            ? { channel_snapshot: detail.channel_snapshot }
            : {}),
          expected_installed_sha256: detail.installed?.sha256 ?? null,
          installed_source: true,
          ...(release.downloaded_bytes ? { resume_download: true } : {}),
          operation_id,
        });
        if (!operation.isCurrent()) return;
        if (result.status === 'paused') {
          setReview({
            detail,
            release: { ...release, downloaded_bytes: result.received_bytes || undefined },
          });
          setNotice(
            result.received_bytes
              ? 'Download paused. Continue when you’re ready.'
              : 'Download interrupted. Try again when you’re ready.',
          );
          return;
        }
        const saved = result.applet;
        if (
          saved.applet_id !== applet_id ||
          saved.sha256 !== release.sha256 ||
          saved.version !== release.version ||
          saved.source?.kind !== 'store' ||
          !sameStoreSource(saved.source, {
            applet_id,
            store_url: detail.store_url,
            channel: storeReleaseChannel(release),
          })
        ) {
          throw new Error('Could not verify the installed release. Check its details again.');
        }
        setReview(undefined);
        updateInstallation({
          store_source: saved.source,
          installed_version: saved.version,
          installed_sha256: saved.sha256,
          restart_required: result.restart_required,
        });
        onInstalled?.({ applet: saved, restart_required: result.restart_required });
        if (!result.restart_required && release.action === 'update') {
          setNotice(`Version ${saved.version} is installed.`);
        }
      } catch (error) {
        if (operation.isCurrent()) {
          setReview(undefined);
          await reload(operation).catch(() => undefined);
        }
        throw error;
      } finally {
        stop();
      }
    });

  const access = review ? newStoreAccess(review.release, review.detail.installed) : [];
  const reviewing_update = review?.release.action === 'update';
  return (
    <div className="applet-channel">
      <dl className="applet-access">
        <div>
          <dt>Installed channel</dt>
          <dd>
            <select
              aria-label="Installed channel"
              value={source.channel}
              disabled={controls_disabled || !!review}
              onChange={(event) => {
                const next = event.currentTarget.value;
                if (next === 'stable' || next === 'nightly') checkRelease(next);
              }}
            >
              <option value="stable">Stable</option>
              <option value="nightly">Nightly</option>
            </select>
          </dd>
        </div>
      </dl>
      <button
        type="button"
        disabled={controls_disabled || !!review}
        onClick={() => checkRelease(source.channel)}
      >
        Check for updates
      </button>
      {phase === 'checking' && <p role="status">Checking releases…</p>}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {review && (
        <div className="applet-channel-review">
          {reviewing_update ? (
            <strong>Update to {review.release.version}?</strong>
          ) : (
            <>
              <strong>Switch to {storeChannelName(storeReleaseChannel(review.release))}?</strong>
              <p>
                Install version {review.release.version} and use this channel for future updates.
              </p>
            </>
          )}
          {review.release.release_notes && (
            <p className="applet-channel-notes">{review.release.release_notes}</p>
          )}
          {!!access.length && (
            <>
              <p>New access in this version</p>
              <ul>
                {access.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </>
          )}
          {phase === 'installing' ? (
            <div className="applet-channel-progress" role="status">
              <progress
                aria-label="Applet download"
                max={progress?.total_bytes || 1}
                value={progress?.total_bytes ? progress.received_bytes : undefined}
              />
              <span>
                {progress ? `${storeSize(progress.received_bytes)} downloaded` : 'Installing…'}
              </span>
            </div>
          ) : (
            <div className="button-row">
              <button
                onClick={() => {
                  setReview(undefined);
                  setNotice('');
                }}
              >
                Cancel
              </button>
              <button
                className="primary"
                disabled={controls_disabled}
                onClick={() => install(review)}
              >
                {review.release.downloaded_bytes !== undefined
                  ? 'Continue download'
                  : reviewing_update
                    ? 'Update applet'
                    : 'Switch and install'}
              </button>
            </div>
          )}
        </div>
      )}
      {restart}
    </div>
  );
}
