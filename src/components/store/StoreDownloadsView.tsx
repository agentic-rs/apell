import type { Ref } from 'react';
import type { StorePendingDownload } from '../../lib/types';
import { Icon } from '../Icon';
import { storeDownloadId, StorePendingDownloadNotice } from './StorePendingDownloadNotice';
import type { StoreDownloads } from './useStoreDownloads';

export function StoreDownloadsView({
  downloads,
  heading_ref,
  return_id,
  return_ref,
  reviews_disabled,
  onReview,
}: {
  downloads: StoreDownloads;
  heading_ref: Ref<HTMLHeadingElement>;
  return_id?: string;
  return_ref: Ref<HTMLButtonElement>;
  reviews_disabled: boolean;
  onReview: (pending: StorePendingDownload) => void;
}) {
  return (
    <section className="store-downloads" aria-labelledby="store-downloads-heading">
      <div className="store-toolbar">
        <h2 ref={heading_ref} id="store-downloads-heading" tabIndex={-1}>
          Downloads
        </h2>
        <button
          className="icon-button"
          type="button"
          aria-label="Refresh downloads"
          title="Refresh downloads"
          disabled={downloads.loading}
          onClick={downloads.refresh}
        >
          <Icon name="reload" />
        </button>
      </div>
      {downloads.error && (
        <div className="store-feedback error" role="alert">
          <strong>Couldn’t check saved downloads</strong>
          <p>{downloads.error}</p>
        </div>
      )}
      {downloads.entries.length ? (
        <ul className="store-list" aria-busy={downloads.loading}>
          {downloads.entries.map((pending) => {
            const id = storeDownloadId(pending);
            return (
              <li key={id}>
                <StorePendingDownloadNotice
                  pending={pending}
                  disabled={reviews_disabled}
                  button_ref={id === return_id ? return_ref : undefined}
                  onReview={() => onReview(pending)}
                />
              </li>
            );
          })}
        </ul>
      ) : downloads.loading ? (
        <p className="status" role="status">
          Checking saved downloads…
        </p>
      ) : !downloads.error ? (
        <p className="status">No paused downloads.</p>
      ) : null}
    </section>
  );
}
