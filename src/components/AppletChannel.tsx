import { useEffect, useRef, useState } from 'react';
import {
  getAppletInstallation,
  getStoreDetail,
  installStoreRelease,
  message,
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
  AppletInstallation,
  AppletSession,
  InstallProgress,
  StoreChannel,
  StoreDetail,
  StoreReleaseOption,
} from '../lib/types';
import { OperationScope, type Operation } from './providers/state';

interface Review {
  detail: StoreDetail;
  release: StoreReleaseOption;
}

type Phase = 'loading' | 'checking' | 'installing';

export function AppletChannel({
  session,
  onInstalling,
  onRestart,
}: {
  session: AppletSession;
  onInstalling: (installing: boolean) => void;
  onRestart: () => void;
}) {
  const [installation, setInstallation] = useState<AppletInstallation>();
  const [review, setReview] = useState<Review>();
  const [phase, setPhase] = useState<Phase>();
  const [progress, setProgress] = useState<InstallProgress>();
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const scope = useRef(new OperationScope());
  const { session_id, view_id } = session;

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

  const load = () =>
    void run('loading', async (operation) => {
      const installed = await getAppletInstallation(session_id, view_id);
      if (operation.isCurrent()) setInstallation(installed);
    });
  useEffect(() => {
    load();
    return () => scope.current.cancel();
  }, [session_id, view_id]);

  const source = installation?.store_source;
  if (!installation) {
    if (!error) return <p role="status">Checking installed channel…</p>;
    return (
      <div className="applet-channel">
        <p role="alert">{error}</p>
        <button onClick={load}>Retry installed channel</button>
      </div>
    );
  }
  if (!source) return null;

  const checkRelease = (channel: StoreChannel) =>
    void run('checking', async (operation) => {
      setReview(undefined);
      const detail = await getStoreDetail(source.store_url, source.applet_id, true);
      if (!operation.isCurrent()) return;
      if (detail.installed_channel !== source.channel || !detail.channel_snapshot) {
        const installed = await getAppletInstallation(session_id, view_id);
        if (operation.isCurrent()) {
          setInstallation(installed);
          throw new Error('The installed channel changed. Choose a channel again.');
        }
        return;
      }
      const recommended = recommendedStoreRelease(detail, channel);
      const release = detail.releases.find(
        (item) => item.version === recommended.version && storeReleaseChannel(item) === channel,
      );
      if (!release) throw new Error(`No ${storeChannelName(channel)} release is available yet.`);
      if (release.action !== 'switch_channel')
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
          channel_snapshot: detail.channel_snapshot,
          expected_installed_sha256: detail.installed?.sha256 ?? null,
          installed_source: true,
          operation_id,
        });
        if (!operation.isCurrent()) return;
        if (result.status === 'paused') {
          setReview({
            detail,
            release: { ...release, downloaded_bytes: result.received_bytes },
          });
          setNotice('Download paused. Continue when you’re ready.');
          return;
        }
        setReview(undefined);
        setInstallation({
          store_source: result.applet.source?.kind === 'store' ? result.applet.source : null,
          installed_version: result.applet.version,
          restart_required: result.restart_required,
        });
      } catch (error) {
        if (operation.isCurrent()) setReview(undefined);
        throw error;
      } finally {
        stop();
      }
    });

  const access = review ? newStoreAccess(review.release, review.detail.installed) : [];
  return (
    <div className="applet-channel">
      <dl className="applet-access">
        <div>
          <dt>Installed channel</dt>
          <dd>
            <select
              aria-label="Installed channel"
              value={source.channel}
              disabled={!!phase || !!review}
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
      {phase === 'checking' && <p role="status">Checking releases…</p>}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {review && (
        <div className="applet-channel-review">
          <strong>Switch to {storeChannelName(storeReleaseChannel(review.release))}?</strong>
          <p>Install version {review.release.version} and use this channel for future updates.</p>
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
              <button className="primary" onClick={() => install(review)}>
                {review.release.downloaded_bytes === undefined
                  ? 'Switch and install'
                  : 'Continue download'}
              </button>
            </div>
          )}
        </div>
      )}
      {installation.restart_required && !review && (
        <>
          <p role="status">
            Version {installation.installed_version} is installed. Restart to use it.
          </p>
          <button className="primary" disabled={!!phase} onClick={onRestart}>
            Restart applet
          </button>
        </>
      )}
    </div>
  );
}
