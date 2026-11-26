import { useId } from 'react';
import { storeChannelName, storeSize } from '../../lib/storeState';
import type { StorePendingDownload } from '../../lib/types';

export function StorePendingDownloadNotice({
  pending,
  onReview,
}: {
  pending: StorePendingDownload;
  onReview: () => void;
}) {
  const id = useId();
  return (
    <button
      type="button"
      className="store-card store-pending-download"
      aria-label={`Review download for ${pending.name}, version ${pending.version}`}
      aria-describedby={`${id}-status ${id}-progress`}
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
      </span>
      <span className="store-pending-download-action" aria-hidden="true">
        Review <span className="store-card-arrow">›</span>
      </span>
    </button>
  );
}
