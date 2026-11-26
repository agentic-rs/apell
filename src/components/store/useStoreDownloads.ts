import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getStorePendingDownloads,
  message,
  nativeAvailable,
  onStoreDownloadsChanged,
} from '../../lib/tauri';
import type { StorePendingDownload } from '../../lib/types';

export interface StoreDownloads {
  entries: StorePendingDownload[];
  loading: boolean;
  error: string;
  refresh: () => void;
}

export function useStoreDownloads(): StoreDownloads {
  const [entries, setEntries] = useState<StorePendingDownload[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const refresh_action = useRef<() => void>(undefined);
  const refresh = useCallback(() => refresh_action.current?.(), []);

  useEffect(() => {
    if (!nativeAvailable()) return;
    let open = true;
    let reading = false;
    let refresh_requested = false;
    const load = async () => {
      if (!open) return;
      if (reading) {
        refresh_requested = true;
        return;
      }
      reading = true;
      setLoading(true);
      try {
        do {
          refresh_requested = false;
          try {
            const result = await getStorePendingDownloads();
            if (open && !refresh_requested) {
              setEntries(result);
              setError('');
            }
          } catch (cause) {
            if (open && !refresh_requested) setError(message(cause));
          }
        } while (open && refresh_requested);
      } finally {
        reading = false;
        if (open) setLoading(false);
      }
    };
    const requestRefresh = () => void load();
    refresh_action.current = requestRefresh;
    const unsubscribe = onStoreDownloadsChanged(requestRefresh).catch(() => undefined);
    void unsubscribe.then(requestRefresh);
    const refreshVisible = () => {
      if (document.visibilityState === 'visible') requestRefresh();
    };
    window.addEventListener('focus', refreshVisible);
    document.addEventListener('visibilitychange', refreshVisible);
    return () => {
      open = false;
      refresh_action.current = undefined;
      window.removeEventListener('focus', refreshVisible);
      document.removeEventListener('visibilitychange', refreshVisible);
      void unsubscribe.then((stop) => stop?.());
    };
  }, []);

  return { entries, loading, error, refresh };
}
