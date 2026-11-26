import { useId, type Ref } from 'react';
import { storeChannelName, storeSize } from '../../lib/storeState';
import type { StorePendingDownload } from '../../lib/types';

export function storeDownloadId(download: StorePendingDownload) {
  const { origin, applet_id, version, channel, sha256 } = download;
  return JSON.stringify([origin, applet_id, version, channel, sha256]);
}

export function StorePendingDownloadNotice({
  pending,
  onReview,
  disabled,
  button_ref,
}: {
  pending: StorePendingDownload;
  onReview: () => void;
  disabled?: boolean;
  button_ref?: Ref<HTMLButtonElement>;
}) {
  const id = useId();
  return (
    <button
      type="button"
      ref={button_ref}
      className="store-card store-pending-download"
      aria-label={`Review download for ${pending.name}, version ${pending.version}`}
      aria-describedby={`${id}-status ${id}-progress ${id}-origin`}
      disabled={disabled}
      onClick={onReview}
    >
      <span className="store-card-copy">
        <span className="store-pending-download-status" id={`${id}-status`}>
          Download paused
        </span>
        <strong>
          <bdi>{pending.name}</bdi> · <bdi>{pending.version}</bdi>
        </strong>
        <span className="store-pending-download-progress" id={`${id}-progress`}>
          {storeChannelName(pending.channel)} · {storeSize(pending.downloaded_bytes)} of{' '}
          {storeSize(pending.size_bytes)} downloaded
        </span>
        <span className="store-pending-download-origin" id={`${id}-origin`}>
          <bdi>{pending.origin}</bdi>
        </span>
      </span>
      <span className="store-pending-download-action" aria-hidden="true">
        Review <span className="store-card-arrow">›</span>
      </span>
    </button>
  );
}
