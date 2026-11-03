import { useState } from 'react';
import type { Manifest } from '../lib/types';
import { Icon } from './Icon';
import { Sheet } from './Sheet';

const permission_labels: Record<string, string> = {
  sqlite: 'Saved app data',
  llm: 'Shared AI',
  resources: 'Downloaded resources',
  device: 'Device information',
  http: 'Network requests',
  fs: 'App files',
  config: 'App preferences',
};

interface AppletControlsProps {
  manifest: Manifest;
  disabled: boolean;
  stopped?: boolean;
  onClose: () => void;
  onReload: () => void;
}

export function AppletControls({
  manifest,
  disabled,
  stopped,
  onClose,
  onReload,
}: AppletControlsProps) {
  const [panel, setPanel] = useState<'menu' | 'about' | null>(null);
  return (
    <>
      <div className="applet-controls" role="group" aria-label="Applet controls">
        <button
          className="icon-button"
          aria-label="More applet actions"
          title="More applet actions"
          aria-haspopup="dialog"
          aria-expanded={panel !== null}
          disabled={disabled}
          onClick={(event) => {
            // Safari does not focus tapped buttons; give the sheet a stable return target.
            event.currentTarget.focus({ preventScroll: true });
            setPanel('menu');
          }}
        >
          <Icon name="more" />
        </button>
        <span className="applet-control-divider" aria-hidden="true" />
        <button
          className="icon-button"
          aria-label="Close applet"
          title="Close applet"
          disabled={disabled}
          onClick={onClose}
        >
          <Icon name="exit" />
        </button>
      </div>
      {panel && (
        <Sheet
          key={panel}
          open
          title={panel === 'about' ? 'About this applet' : manifest.name}
          onClose={() => setPanel(null)}
        >
          {panel === 'menu' ? (
            <div className="applet-menu-actions">
              <button
                className="applet-menu-action"
                disabled={disabled || stopped}
                onClick={() => {
                  setPanel(null);
                  onReload();
                }}
              >
                <Icon name="reload" />
                Reload applet
              </button>
              <button className="applet-menu-action" onClick={() => setPanel('about')}>
                <Icon name="info" />
                About this applet
              </button>
            </div>
          ) : (
            <div className="applet-about">
              <h3>{manifest.name}</h3>
              <p>Version {manifest.version}</p>
              <h4>Access</h4>
              {manifest.permissions.length ? (
                <ul>
                  {manifest.permissions.map((permission) => (
                    <li key={permission}>{permission_labels[permission] ?? permission}</li>
                  ))}
                </ul>
              ) : (
                <p>No shared services requested.</p>
              )}
            </div>
          )}
        </Sheet>
      )}
    </>
  );
}
