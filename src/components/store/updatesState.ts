import { installedStoreChannel } from '../../lib/storeState';
import type {
  AppletListReport,
  StoreUpdateEntry,
  StoreUpdateInstallation,
  StoreUpdateSource,
  StoreUpdatesReport,
} from '../../lib/types';

interface UpdateGroup extends Omit<StoreUpdateSource, 'unreachable_installations'> {
  saved_updates: StoreUpdateEntry[];
}

export interface StoreUpdateResults {
  fresh: boolean;
  sources: UpdateGroup[];
}

function matchesInstallation(
  store_url: string,
  entry: StoreUpdateEntry,
  installation: StoreUpdateInstallation | undefined,
): boolean {
  const { applet } = entry;
  return (
    installation?.sha256 === applet.sha256 &&
    installation.channel === installedStoreChannel(store_url, applet.applet_id, applet)
  );
}

export function savePreviousUpdates(previous?: StoreUpdateResults): StoreUpdateResults | undefined {
  return (
    previous && {
      fresh: false,
      sources: previous.sources.map((source) => ({
        ...source,
        updates: [],
        saved_updates: [...source.updates, ...source.saved_updates],
      })),
    }
  );
}

export function applyUpdateCheck(
  report: StoreUpdatesReport,
  previous?: StoreUpdateResults,
): StoreUpdateResults {
  const saved_by_origin = new Map(previous?.sources.map((source) => [source.store_url, source]));
  return {
    fresh: true,
    sources: report.sources.map(({ unreachable_installations, ...source }) => {
      const earlier = saved_by_origin.get(source.store_url);
      const installed = new Map(
        unreachable_installations?.map((applet) => [applet.applet_id, applet]),
      );
      const saved_updates =
        source.error && earlier
          ? [...earlier.updates, ...earlier.saved_updates].filter((entry) =>
              matchesInstallation(source.store_url, entry, installed.get(entry.applet.applet_id)),
            )
          : [];
      return { ...source, saved_updates };
    }),
  };
}

export function verifySavedUpdates(
  current: StoreUpdateResults | undefined,
  report: AppletListReport,
): StoreUpdateResults | undefined {
  const installed = new Map(report.applets.map((applet) => [applet.applet_id, applet]));
  return (
    current && {
      fresh: current.fresh,
      sources: current.sources.map((source) => ({
        ...source,
        saved_updates: source.saved_updates.filter((entry) => {
          const applet = installed.get(entry.applet.applet_id);
          const channel = installedStoreChannel(
            source.store_url,
            entry.applet.applet_id,
            applet ?? null,
          );
          return (
            applet &&
            channel &&
            matchesInstallation(source.store_url, entry, {
              applet_id: applet.applet_id,
              sha256: applet.sha256,
              channel,
            })
          );
        }),
      })),
    }
  );
}
