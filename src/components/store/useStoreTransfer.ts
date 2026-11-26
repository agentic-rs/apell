import { useEffect, useRef, useState } from 'react';
import {
  message,
  onInstallProgress,
  onStoreTransferStarted,
  pauseStoreDownload,
} from '../../lib/tauri';
import type { InstallProgress } from '../../lib/types';

export interface StoreTransferControl {
  started: boolean;
  pausing: boolean;
  can_pause: boolean;
  error: string;
  pause: () => void;
}

export function useStoreTransfer() {
  const [progress, setProgress] = useState<InstallProgress>();
  const [started, setStarted] = useState(false);
  const [pausing, setPausing] = useState(false);
  const [error, setError] = useState('');
  const current = useRef<{ operation_id: string; pause_requested: boolean }>(undefined);

  useEffect(
    () => () => {
      current.current = undefined;
    },
    [],
  );

  const run = async <T>(operation_id: string, invoke: () => Promise<T>): Promise<T> => {
    const attempt = { operation_id, pause_requested: false };
    current.current = attempt;
    setStarted(false);
    setProgress(undefined);
    setPausing(false);
    setError('');
    // Register before invoking so even a pause during Store metadata review
    // targets a native transfer that is already listening for it.
    const unsubscribe = await Promise.all([
      onStoreTransferStarted((started_id) => {
        if (current.current === attempt && started_id === operation_id) setStarted(true);
      }).catch(() => undefined),
      onInstallProgress((event) => {
        if (current.current === attempt && event.operation_id === operation_id) setProgress(event);
      }).catch(() => undefined),
    ]);
    try {
      if (current.current !== attempt) throw new Error('The download view was closed.');
      return await invoke();
    } finally {
      for (const stop of unsubscribe) stop?.();
      if (current.current === attempt) {
        current.current = undefined;
        setStarted(false);
        setProgress(undefined);
        setPausing(false);
        setError('');
      }
    }
  };

  const can_pause =
    started &&
    !pausing &&
    !(progress?.total_bytes && progress.received_bytes >= progress.total_bytes);
  const pause = async () => {
    const attempt = current.current;
    if (!attempt || attempt.pause_requested || !can_pause) return;
    attempt.pause_requested = true;
    setPausing(true);
    setError('');
    try {
      // The command only requests a pause. The original install reports
      // the checkpoint or publishes the verified package if it finished first.
      const accepted = await pauseStoreDownload(attempt.operation_id);
      if (!accepted && current.current === attempt) {
        setStarted(false);
        setPausing(false);
      }
    } catch (cause) {
      if (current.current === attempt) {
        attempt.pause_requested = false;
        setPausing(false);
        setError(message(cause));
      }
    }
  };

  const control: StoreTransferControl = {
    started,
    pausing,
    can_pause,
    error,
    pause: () => void pause(),
  };
  return { run, progress, control };
}
