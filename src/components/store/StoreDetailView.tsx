import { useState } from 'react';
import type { InstallProgress, StoreDetail, StoreReleaseOption } from '../../lib/types';
import {
  newStoreAccess,
  permissionName,
  recommendedStoreRelease,
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
  busy,
  installing,
  progress,
  onVersion,
  onInstall,
  onOpen,
  onBack,
}: {
  detail: StoreDetail;
  selected_version: string;
  busy: boolean;
  installing: boolean;
  progress?: InstallProgress;
  onVersion: (version: string) => void;
  onInstall: (release: StoreReleaseOption) => void;
  onOpen: () => void;
  onBack: () => void;
}) {
  const [confirm_switch, setConfirmSwitch] = useState(false);
  const release = detail.releases.find((item) => item.version === selected_version);
  const recommended = recommendedStoreRelease(detail);
  const new_access = release ? newStoreAccess(release, detail.installed) : [];
  const previous = detail.switch_snapshot;
  const previous_source = previous ? storeSourceLabel(previous.source) : null;
  const can_switch = release?.action === 'switch_source' && !!previous;
  const can_install =
    release && (release.action === 'install' || release.action === 'update' || can_switch);
  return (
    <section className="store-detail" aria-label={detail.applet.name}>
      <button className="store-back" disabled={busy} onClick={onBack}>
        <Icon name="back" /> All applets
      </button>
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
              ? `Installed · ${detail.installed.version}${previous_source ? ` · From ${previous_source}` : ''}`
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
            Version
            <select
              value={selected_version}
              disabled={busy}
              onChange={(event) => onVersion(event.target.value)}
            >
              {detail.releases.map((item) => (
                <option key={item.version} value={item.version}>
                  {item.version} · {item.prerelease ? 'Preview' : 'Stable'}
                  {item.version === recommended.version ? ' · Recommended' : ''}
                </option>
              ))}
            </select>
          </label>
          {release && (
            <>
              <div className="store-release-meta">
                <span className={`store-badge${release.prerelease ? ' store-badge-preview' : ''}`}>
                  {release.prerelease ? 'Preview' : 'Stable'}
                </span>
                <span>
                  {storeSize(release.size_bytes)} · {storeDate(release.published_at)}
                </span>
              </div>
              {release.prerelease && (
                <p className="store-preview-note">
                  This is a preview release and may be less stable.
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
                    disabled={busy}
                    onClick={() => (can_switch ? setConfirmSwitch(true) : onInstall(release))}
                  >
                    {installing
                      ? 'Installing…'
                      : can_switch
                        ? 'Switch to this Store'
                        : release.action === 'update'
                          ? `Update to ${release.version}`
                          : 'Install applet'}
                  </button>
                )}
                {detail.installed && (
                  <button
                    className={can_install ? '' : 'store-primary'}
                    disabled={busy}
                    onClick={onOpen}
                  >
                    {busy && !installing ? 'Opening…' : 'Open applet'}
                  </button>
                )}
                {!detail.installed && !can_install && (
                  <button disabled>
                    {release.action === 'older' ? 'Older version' : 'Unavailable'}
                  </button>
                )}
              </div>
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
                <p className="store-action-reason">This version is installed.</p>
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
      {release && can_switch && (
        <Sheet
          open={confirm_switch}
          title="Switch installation source?"
          onClose={() => setConfirmSwitch(false)}
        >
          <p className="store-source-review">
            Install {detail.applet.name} {release.version} from {detail.store_url} instead of{' '}
            {previous_source}. Future updates will come from this store.
          </p>
          <p>
            This copy will have access to the applet’s saved data, including secure storage if
            declared. Remembered device permissions from the current source won’t carry over.
          </p>
          <div className="store-form-actions">
            <button onClick={() => setConfirmSwitch(false)}>Cancel</button>
            <button
              className="store-primary"
              disabled={busy}
              onClick={() => {
                setConfirmSwitch(false);
                onInstall(release);
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
