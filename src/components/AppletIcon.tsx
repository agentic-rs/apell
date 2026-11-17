import { useState } from 'react';
import companion_icon from '../../assets/companion.svg';
import type { InstalledApplet } from '../lib/types';

type AppletIdentity = Pick<InstalledApplet, 'applet_id' | 'name' | 'icon_data_url'>;

export function appletIconSource(
  applet: AppletIdentity,
  failed_sources: readonly string[] = [],
): string | null {
  const sources = [applet.icon_data_url, applet.applet_id === 'chatbot' ? companion_icon : null];
  return sources.find((source) => source && !failed_sources.includes(source)) ?? null;
}

export function AppletIcon({ applet }: { applet: AppletIdentity }) {
  const [failed_sources, setFailedSources] = useState<string[]>([]);
  const source = appletIconSource(applet, failed_sources);
  if (!source) {
    return (
      <span className="applet-initial">
        {Array.from(applet.name.trim())[0]?.toLocaleUpperCase() || 'a'}
      </span>
    );
  }
  return (
    <img
      src={source}
      alt=""
      onError={() => setFailedSources((previous) => [...previous, source])}
    />
  );
}
