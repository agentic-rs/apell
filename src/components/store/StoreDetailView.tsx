import { useEffect, useState } from 'react';
import type {
  InstallProgress,
  StoreChannel,
  StoreDetail,
  StoreReleaseOption,
} from '../../lib/types';
import {
  newStoreAccess,
  permissionName,
  recommendedStoreRelease,
  storeChannelName,
  storeReleaseChannel,
  storeDate,
  storeSize,
  storeSourceLabel,
} from '../../lib/storeState';
import { Icon } from '../Icon';
import { Sheet } from '../Sheet';
import { StoreIcon } from './StoreIcon';

export function StoreDetailView({
  detail,
  selected_version,
  fresh,
  refreshing,
  busy,
  installing,
  restarting,
  progress,
  onVersion,
  onInstall,
  onOpen,
  onRefresh,
  onBack,
}: {
  detail: StoreDetail;
  selected_version: string;
  fresh: boolean;
  refreshing: boolean;
  busy: boolean;
  installing: boolean;
  restarting: boolean;
  progress?: InstallProgress;
  onVersion: (version: string) => void;
  onInstall: (release: StoreReleaseOption, confirmed_channel_switch?: boolean) => void;
  onOpen: () => void;
  onRefresh: () => void;
  onBack: () => void;
}) {
  const [confirm_switch, setConfirmSwitch] = useState(false);
  useEffect(() => {
    if (!fresh) setConfirmSwitch(false);
  }, [fresh]);
  const release = detail.releases.find((item) => item.version === selected_version);
  const recommended = recommendedStoreRelease(detail, release && storeReleaseChannel(release));
  const channel = storeReleaseChannel(release ?? recommended);
  const channel_name = storeChannelName(channel);
  const versions = detail.releases.filter((item) => storeReleaseChannel(item) === channel);
  const changeChannel = (next: StoreChannel) => {
    const latest = recommendedStoreRelease(detail, next);
    if (storeReleaseChannel(latest) === next) onVersion(latest.version);
  };
  const new_access = release ? newStoreAccess(release, detail.installed) : [];
  const previous = detail.switch_snapshot;
  const previous_source = previous ? storeSourceLabel(previous.source) : null;
  const can_switch_source = release?.action === 'switch_source' && !!previous;
  const can_switch_channel = release?.action === 'switch_channel' && !!detail.channel_snapshot;
  const confirm_required = can_switch_source || can_switch_channel;
  const can_install =
    release && (release.action === 'install' || release.action === 'update' || confirm_required);
  return (
    <section className="store-detail" aria-label={detail.applet.name} aria-busy={refreshing}>
      <div className="store-toolbar">
        <button className="store-back" disabled={busy} onClick={onBack}>
          <Icon name="back" /> All applets
        </button>
        <button
          className="icon-button"
          aria-label="Refresh applet details"
          title="Refresh applet details"
          disabled={busy || refreshing}
          onClick={onRefresh}
        >
          <Icon name="reload" />
        </button>
      </div>
      <div className="store-detail-heading">
        <StoreIcon
          store_url={detail.store_url}
          icon_url={detail.applet.icon_url}
          name={detail.applet.name}
          large
        />
        <div>
          <h2>{detail.applet.name}</h2>
          <p>
            {detail.installed
              ? `Installed · ${detail.installed.version}${detail.installed_channel ? ` · ${storeChannelName(detail.installed_channel)}` : ''}${previous_source ? ` · From ${previous_source}` : ''}`
              : previous_source
                ? `Saved data from ${previous_source}`
                : 'An applet for Apell'}
          </p>
        </div>
      </div>
      <p className="store-description">{detail.applet.description}</p>
      <div className="store-detail-layout">
        <div className="store-download-panel">
          <label>
            Release channel
            <select
              value={channel}
              disabled={busy}
              onChange={(event) =>
                changeChannel(event.target.value === 'nightly' ? 'nightly' : 'stable')
              }
            >
              <option value="stable" disabled={!detail.releases.some((item) => !item.prerelease)}>
                Stable
              </option>
              <option value="nightly" disabled={!detail.releases.some((item) => item.prerelease)}>
                Nightly
              </option>
            </select>
          </label>
          <label>
            Version
            <select
              value={selected_version}
              disabled={busy}
              onChange={(event) => onVersion(event.target.value)}
            >
              {versions.map((item) => (
                <option key={item.version} value={item.version}>
                  {item.version}
                  {item.version === recommended.version ? ' · Recommended' : ''}
                </option>
              ))}
            </select>
          </label>
          {release && (
            <>
              <div className="store-release-meta">
                <span className={`store-badge${release.prerelease ? ' store-badge-preview' : ''}`}>
                  {channel_name}
                </span>
                <span>
                  {storeSize(release.size_bytes)} · {storeDate(release.published_at)}
                </span>
              </div>
              {release.prerelease && (
                <p className="store-preview-note">
                  Nightly builds may be less stable than Stable releases.
                </p>
              )}
              {!!new_access.length && can_install && (
                <div className="store-new-access">
                  <strong>New access in this version</strong>
                  <ul>
                    {new_access.map((name) => (
                      <li key={name}>{name}</li>
                    ))}
                  </ul>
                  <p>Device permissions still require your approval when used.</p>
                </div>
              )}
              <div className="store-install-actions">
                {can_install && (
                  <button
                    className="store-primary"
                    disabled={busy || !fresh}
                    onClick={() => (confirm_required ? setConfirmSwitch(true) : onInstall(release))}
                  >
                    {installing
                      ? 'Installing…'
                      : release.downloaded_bytes !== undefined
                        ? 'Continue download'
                        : can_switch_source
                          ? 'Switch to this Store'
                          : can_switch_channel
                            ? `Switch to ${channel_name}`
                            : release.action === 'update'
                              ? `Update to ${release.version}`
                              : 'Install applet'}
                  </button>
                )}
                {detail.installed && (
                  <button
                    className={can_install ? '' : 'store-primary'}
                    disabled={busy || !fresh}
                    onClick={onOpen}
                  >
                    {detail.restart_required
                      ? restarting
                        ? 'Restarting…'
                        : 'Restart applet'
                      : busy && !installing
                        ? 'Opening…'
                        : 'Open applet'}
                  </button>
                )}
                {fresh && !detail.installed && !can_install && (
                  <button disabled>
                    {release.action === 'older' ? 'Older version' : 'Unavailable'}
                  </button>
                )}
              </div>
              {!fresh && (
                <p className="store-action-reason" role="status">
                  {refreshing
                    ? 'Checking this applet’s current releases…'
                    : 'Refresh these details to check what can be installed.'}
                </p>
              )}
              {!installing && !!release.downloaded_bytes && can_install && (
                <p className="store-action-reason">
                  {storeSize(release.downloaded_bytes)} of {storeSize(release.size_bytes)}{' '}
                  downloaded.
                </p>
              )}
              {installing && (
                <div className="store-progress" role="status">
                  <progress
                    aria-label="Applet download"
                    max={progress?.total_bytes || 1}
                    value={progress?.total_bytes ? progress.received_bytes : undefined}
                  />
                  <span>
                    {progress
                      ? `${storeSize(progress.received_bytes)}${progress.total_bytes ? ` of ${storeSize(progress.total_bytes)}` : ''} downloaded`
                      : 'Downloading and checking the package…'}
                  </span>
                </div>
              )}
              {release.action_reason && (
                <p className="store-action-reason">{release.action_reason}</p>
              )}
              {release.action === 'installed' && (
                <p className="store-action-reason">
                  {detail.restart_required
                    ? 'Restart the applet to use the installed version.'
                    : 'This version is installed.'}
                </p>
              )}
            </>
          )}
        </div>
        {release && (
          <div className="store-release-details">
            {release.release_notes && (
              <section>
                <h3>What’s new</h3>
                <p className="store-release-notes">{release.release_notes}</p>
              </section>
            )}
            <section>
              <h3>Declared access</h3>
              <ul className="store-permissions">
                {release.permissions.map((permission) => (
                  <li key={permission}>
                    {permissionName(permission)}
                    {detail.installed && !detail.installed.permissions.includes(permission) && (
                      <small>New</small>
                    )}
                  </li>
                ))}
                <li>
                  {release.network ? 'Network access' : 'Network disabled'}
                  {detail.installed && release.network && !detail.installed.network && (
                    <small>New</small>
                  )}
                </li>
              </ul>
              <p className="store-access-note">
                Device access is requested when used. Updates keep this applet’s files and saved
                data.
              </p>
            </section>
            <details className="store-package-info">
              <summary>Package details</summary>
              <dl>
                <div>
                  <dt>Applet</dt>
                  <dd>{detail.applet.applet_id}</dd>
                </div>
                <div>
                  <dt>Version</dt>
                  <dd>{release.version}</dd>
                </div>
                <div>
                  <dt>SHA-256</dt>
                  <dd>
                    <code>{release.sha256}</code>
                  </dd>
                </div>
              </dl>
            </details>
          </div>
        )}
      </div>
      {fresh && release && confirm_required && (
        <Sheet
          open={confirm_switch}
          title={can_switch_source ? 'Switch installation source?' : `Switch to ${channel_name}?`}
          onClose={() => setConfirmSwitch(false)}
        >
          {can_switch_source ? (
            <>
              <p className="store-source-review">
                Install {detail.applet.name} {release.version} from {detail.store_url} instead of{' '}
                {previous_source}. Future {channel_name} updates will come from this store.
              </p>
              <p>
                This copy will have access to the applet’s saved data, including secure storage if
                declared. Remembered device permissions from the current source won’t carry over.
              </p>
            </>
          ) : (
            <p className="store-source-review">
              Install {detail.applet.name} {release.version} and follow {channel_name} for future
              updates. Your saved applet data stays on this device.
            </p>
          )}
          <div className="store-form-actions">
            <button onClick={() => setConfirmSwitch(false)}>Cancel</button>
            <button
              className="store-primary"
              disabled={busy}
              onClick={() => {
                setConfirmSwitch(false);
                onInstall(release, can_switch_channel);
              }}
            >
              Switch and install
            </button>
          </div>
        </Sheet>
      )}
    </section>
  );
}
