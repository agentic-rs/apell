import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getSavedStoreUpdates,
  getStoreUpdates,
  message,
  nativeAvailable,
  onAppletInstallationChanged,
  retryStoreUpdates,
} from '../../lib/tauri';
import type { StoreUpdateResults, StoreUpdateSource } from '../../lib/types';

const RECENT_CHECK_MS = 60_000;

export type StoreUpdates = ReturnType<typeof useStoreUpdates>;

export function useStoreUpdates() {
  const [report, setReport] = useState<StoreUpdateResults>();
  const [loading, setLoading] = useState(false);
  const [retrying_store, setRetryingStore] = useState<string>();
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const scope = useRef({
    visible: false,
    running: false,
    observing: false,
    revision: 0,
    last_check: undefined as { revision: number; time: number } | undefined,
  });

  const check = useCallback(async function checkUpdates() {
    const current = scope.current;
    if (!current.visible || current.running) return;
    current.running = true;
    current.last_check = undefined;
    const revision = current.revision;
    const isCurrent = () => current.revision === revision;
    setLoading(true);
    setNotice('');
    setError('');
    setReport((previous) => previous && { ...previous, fresh: false });
    let checked = false;
    const saved = (async () => {
      try {
        const result = await getSavedStoreUpdates();
        if (isCurrent() && !checked) setReport({ ...result, fresh: false });
      } catch {
        // The live check can still succeed when saved results are unavailable.
      }
    })();
    try {
      const result = await getStoreUpdates();
      checked = true;
      if (isCurrent()) {
        current.last_check = { revision, time: performance.now() };
        setReport({ ...result, fresh: true });
      }
    } catch (cause) {
      if (isCurrent()) {
        await saved;
        if (isCurrent()) setError(message(cause));
      }
    } finally {
      current.running = false;
      setLoading(false);
      if (!isCurrent() && current.visible) void checkUpdates();
    }
  }, []);

  const watch = useCallback(() => {
    const current = scope.current;
    current.visible = true;
    const last = current.last_check;
    if (
      !last ||
      !current.observing ||
      last.revision !== current.revision ||
      performance.now() - last.time >= RECENT_CHECK_MS
    ) {
      void check();
    }
    return () => {
      current.visible = false;
    };
  }, [check]);

  const invalidate = useCallback(
    (applet_id?: string) => {
      scope.current.revision++;
      setNotice('');
      setReport((previous) =>
        applet_id && previous
          ? {
              fresh: false,
              sources: previous.sources.map((source) => ({
                ...source,
                updates: source.updates.filter(({ applet }) => applet.applet_id !== applet_id),
                saved_updates: source.saved_updates.filter(
                  ({ applet }) => applet.applet_id !== applet_id,
                ),
                unresolved_applets: source.unresolved_applets.filter(
                  ({ applet }) => applet.applet_id !== applet_id,
                ),
              })),
            }
          : undefined,
      );
      void check();
    },
    [check],
  );

  useEffect(() => {
    if (!nativeAvailable()) return;
    let open = true;
    const subscription = onAppletInstallationChanged((applet_id) => {
      if (open) invalidate(applet_id);
    })
      .then((stop) => {
        if (open) {
          const current = scope.current;
          current.observing = true;
          // A check started before listening could have missed an installation change.
          if (current.running || current.last_check) invalidate();
        }
        return stop;
      })
      .catch(() => undefined);
    return () => {
      open = false;
      scope.current.observing = false;
      void subscription.then((stop) => stop?.());
    };
  }, [invalidate]);

  const retrySource = async (store_url: string) => {
    const current = scope.current;
    if (!current.visible || current.running || !report?.fresh) return;
    const previous = report.sources.find((source) => source.store_url === store_url);
    if (!previous || (!previous.error && previous.checked_count === previous.installed_count))
      return;
    current.running = true;
    const revision = current.revision;
    const isCurrent = () => current.revision === revision;
    const replace = (source: StoreUpdateSource | null) => {
      setReport(
        (report) =>
          report && {
            ...report,
            sources: report.sources.flatMap((entry) =>
              entry.store_url === store_url ? (source ? [source] : []) : [entry],
            ),
          },
      );
    };
    setRetryingStore(store_url);
    setNotice('');
    try {
      const source = await retryStoreUpdates(store_url);
      if (!isCurrent()) return;
      if (source && source.store_url !== store_url)
        throw new Error('Could not verify this Store’s update check.');
      replace(source);
      if (!source) setNotice('No installed applets use this Store.');
      else if (!source.error && source.checked_count === source.installed_count) {
        setNotice(
          source.updates.length
            ? `Checked updates from ${store_url}.`
            : `No updates available from ${store_url}.`,
        );
      }
    } catch (cause) {
      if (!isCurrent()) return;
      replace({
        ...previous,
        checked_count: 0,
        updates: [],
        saved_updates: [...previous.updates, ...previous.saved_updates],
        unresolved_applets: previous.unresolved_applets.map(({ applet }) => ({
          applet,
          reason: 'not_checked',
        })),
        error: message(cause),
      });
    } finally {
      current.running = false;
      setRetryingStore(undefined);
      if (!isCurrent() && current.visible) void check();
    }
  };

  return {
    report,
    loading,
    retrying_store,
    notice,
    error,
    watch,
    refresh: check,
    retrySource,
    invalidate,
  };
}
