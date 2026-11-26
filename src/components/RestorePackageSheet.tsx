import { useEffect, useId, useRef, useState } from 'react';
import { readPackageFile } from '../lib/packageFile';
import { storeChannelName, storeSize } from '../lib/storeState';
import {
  discardStoreDownload,
  message,
  onInstallProgress,
  restoreAppletPackage,
  restoreStorePackage,
} from '../lib/tauri';
import type { AppletInstallResult, StorePendingDownload, StoreSwitchSnapshot } from '../lib/types';
import { AppletPackagePicker } from './AppletPackagePicker';
import { Icon } from './Icon';
import { Sheet } from './Sheet';
import type { StoreDownloads } from './store/useStoreDownloads';

interface RestoreTarget {
  applet_id: string;
  name: string;
  expected: StoreSwitchSnapshot;
}

export function RestorePackageSheet({
  target,
  downloads,
  pending_download,
  onBusyChange,
  onClose,
  onRestored,
}: {
  target: RestoreTarget;
  downloads: StoreDownloads;
  pending_download?: StorePendingDownload;
  onBusyChange: (busy: boolean) => void;
  onClose: (reason?: 'discarded') => void;
  onRestored: (result: AppletInstallResult) => void | Promise<void>;
}) {
  const { applet_id, expected, name } = target;
  const source = expected.source?.kind === 'store' ? expected.source : undefined;
  const title_id = useId();
  const source_id = useId();
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<{ text: string; error?: boolean }>();
  const [discard_target, setDiscardTarget] = useState<StorePendingDownload>();
  const [discarded_version, setDiscardedVersion] = useState<string>();
  const running = useRef(false);
  const download_operation = useRef<string>(undefined);
  const discard_button = useRef<HTMLButtonElement>(null);
  const store_button = useRef<HTMLButtonElement>(null);
  const restore_discard_focus = useRef(false);
  const saved =
    source &&
    downloads.entries.find((entry) => {
      const repair = entry.restore_snapshot;
      return (
        entry.applet_id === applet_id &&
        entry.origin === source.store_url &&
        entry.channel === source.channel &&
        entry.sha256 === expected.sha256 &&
        entry.version !== discarded_version &&
        (!pending_download || entry.version === pending_download.version) &&
        repair?.sha256 === expected.sha256 &&
        repair.removed === expected.removed &&
        repair.source?.kind === 'store' &&
        repair.source.applet_id === applet_id &&
        repair.source.channel === source.channel &&
        repair.source.store_url === source.store_url
      );
    });
  const reviewed_download = saved ?? pending_download;

  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    void onInstallProgress((event) => {
      if (
        disposed ||
        !download_operation.current ||
        event.operation_id !== download_operation.current
      )
        return;
      const received = event.received_bytes ? storeSize(event.received_bytes) : '0 KB';
      const total = event.total_bytes ? ` of ${storeSize(event.total_bytes)}` : '';
      setFeedback({ text: `Downloading… ${received}${total}` });
    })
      .then((unsubscribe) => {
        if (disposed) unsubscribe();
        else stop = unsubscribe;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  useEffect(() => {
    if (discard_target || busy || !restore_discard_focus.current) return;
    restore_discard_focus.current = false;
    (discard_button.current ?? store_button.current)?.focus({ preventScroll: true });
  }, [busy, discard_target]);

  const run = async (label: string, action: () => Promise<void>, operation_id?: string) => {
    if (running.current) return;
    running.current = true;
    download_operation.current = operation_id;
    onBusyChange(true);
    setBusy(true);
    setFeedback({ text: label });
    try {
      await action();
    } catch (error) {
      setFeedback({ text: message(error), error: true });
    } finally {
      download_operation.current = undefined;
      downloads.refresh();
      setBusy(false);
      onBusyChange(false);
      running.current = false;
    }
  };
  const useFile = (file: File) =>
    void run('Restoring package…', async () => {
      const result = await restoreAppletPackage(applet_id, expected, await readPackageFile(file));
      await onRestored(result);
    });
  const download = () => {
    if (pending_download && !saved) return;
    const operation_id = crypto.randomUUID();
    void run(
      'Checking the original Store…',
      async () => {
        setDiscardedVersion(undefined);
        const result = await restoreStorePackage(applet_id, expected, operation_id, saved);
        download_operation.current = undefined;
        if (result.status === 'restored') {
          await onRestored(result);
        } else {
          setFeedback({
            text: result.received_bytes
              ? `Download paused. ${storeSize(result.received_bytes)} saved.`
              : 'Download paused. Try again to download the package.',
          });
        }
      },
      operation_id,
    );
  };
  const keepDownload = () => {
    restore_discard_focus.current = true;
    setDiscardTarget(undefined);
  };
  const discard = (entry: StorePendingDownload) =>
    void run('Discarding saved download…', async () => {
      const discarded = await discardStoreDownload(entry);
      setDiscardedVersion(entry.version);
      if (pending_download) onClose('discarded');
      else {
        keepDownload();
        setFeedback({
          text: discarded
            ? 'Saved download discarded.'
            : 'The saved download is no longer available.',
        });
      }
    });
  const current_feedback =
    feedback ??
    (pending_download && !saved && !downloads.loading
      ? { text: 'This saved download is no longer available.' }
      : undefined);

  return (
    <Sheet
      open
      title={discard_target ? 'Discard saved download?' : 'Restore package'}
      dismissible={!busy}
      onClose={() => {
        if (running.current) return;
        if (discard_target) keepDownload();
        else onClose();
      }}
    >
      {discard_target ? (
        <>
          <p>
            This clears the saved download for{' '}
            <strong>
              {name} {discard_target.version}
            </strong>
            . You can download the package again or choose the original file.
          </p>
          <div className="button-row">
            <button autoFocus disabled={busy} onClick={keepDownload}>
              Keep download
            </button>
            <button className="danger" disabled={busy} onClick={() => discard(discard_target)}>
              Discard
            </button>
          </div>
        </>
      ) : (
        <>
          <p>
            Restore the original package for <strong>{name}</strong> and keep its saved data and
            permissions.
          </p>
          {source && (
            <button
              ref={store_button}
              type="button"
              className="install-option"
              aria-labelledby={title_id}
              aria-describedby={source_id}
              disabled={busy || (!!pending_download && !saved)}
              onClick={download}
            >
              <span className="option-icon" aria-hidden="true">
                <Icon name="download" />
              </span>
              <span>
                <strong id={title_id}>
                  {saved || pending_download
                    ? 'Continue repair download'
                    : 'Download from original Store'}
                </strong>
                <small id={source_id}>
                  <bdi>{source.store_url}</bdi> · {storeChannelName(source.channel)}
                  {reviewed_download && (
                    <>
                      {' '}
                      · <bdi>{reviewed_download.version}</bdi>
                    </>
                  )}
                  {saved &&
                    ` · ${storeSize(saved.downloaded_bytes)} of ${storeSize(saved.size_bytes)} downloaded`}
                </small>
              </span>
            </button>
          )}
          <AppletPackagePicker
            title="Choose original .apel file"
            label="Choose original .apel file"
            disabled={busy}
            onFile={useFile}
          />
          {saved && (
            <div className="button-row restore-discard-actions">
              <button
                type="button"
                ref={discard_button}
                disabled={busy}
                onClick={() => {
                  setFeedback(undefined);
                  setDiscardTarget(saved);
                }}
              >
                Discard saved download
              </button>
            </div>
          )}
        </>
      )}
      {current_feedback && (
        <p
          className={`status${current_feedback.error ? ' error' : ''}`}
          role={current_feedback.error ? 'alert' : 'status'}
        >
          {current_feedback.text}
        </p>
      )}
    </Sheet>
  );
}
