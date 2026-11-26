import { useEffect, useId, useState } from 'react';
import type { InstalledApplet } from '../lib/types';
import { AppletIcon } from './AppletIcon';
import { Icon } from './Icon';

/** A held or dragged pointer can still produce a click when it is released. */
export class AppletPress {
  private start?: { x: number; y: number };
  private timer?: ReturnType<typeof setTimeout>;
  suppressClick = false;

  begin(x: number, y: number, onHold: () => void) {
    this.end();
    this.suppressClick = false;
    this.start = { x, y };
    this.timer = setTimeout(() => {
      this.cancel();
      onHold();
    }, 500);
  }

  move(x: number, y: number) {
    if (this.start && Math.hypot(x - this.start.x, y - this.start.y) > 10) this.cancel();
  }

  end() {
    clearTimeout(this.timer);
    this.start = undefined;
  }

  cancel() {
    this.end();
    this.suppressClick = true;
  }
}

interface AppletTileProps {
  applet: InstalledApplet;
  disabled: boolean;
  launching: boolean;
  restart_required: boolean;
  onOpen: () => void;
  onActions: () => void;
}

export function AppletTile({
  applet,
  disabled,
  launching,
  restart_required,
  onOpen,
  onActions,
}: AppletTileProps) {
  const [press] = useState(() => new AppletPress());
  const restart_description = useId();
  useEffect(() => () => press.cancel(), [press, disabled]);

  const showActions = (button: HTMLButtonElement) => {
    if (button.disabled) return;
    button.focus({ preventScroll: true });
    onActions();
  };

  return (
    <div className="applet-tile">
      <button
        className="applet-icon-button"
        disabled={disabled}
        aria-label={`Open ${applet.name}`}
        aria-describedby={restart_required ? restart_description : undefined}
        aria-keyshortcuts="Shift+F10"
        onPointerDown={(event) => {
          if (!event.isPrimary || event.button !== 0) {
            press.cancel();
            return;
          }
          const button = event.currentTarget;
          button.setPointerCapture(event.pointerId);
          press.begin(event.clientX, event.clientY, () => showActions(button));
        }}
        onPointerMove={(event) => press.move(event.clientX, event.clientY)}
        onPointerUp={() => press.end()}
        onPointerCancel={() => press.cancel()}
        onLostPointerCapture={() => press.end()}
        onContextMenu={(event) => {
          event.preventDefault();
          press.cancel();
          showActions(event.currentTarget);
        }}
        onKeyDown={(event) => {
          if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
            event.preventDefault();
            showActions(event.currentTarget);
          }
        }}
        onClick={(event) => {
          if (event.detail === 0 || !press.suppressClick) onOpen();
        }}
      >
        <span className="applet-icon" aria-hidden="true">
          {launching ? <span className="spinner" /> : <AppletIcon applet={applet} />}
          {restart_required && (
            <span className="applet-restart-badge">
              <Icon name="reload" />
            </span>
          )}
        </span>
        <span className="applet-name">{applet.name}</span>
        {restart_required && (
          <span className="visually-hidden" id={restart_description}>
            Restart available.
          </span>
        )}
      </button>
      <button
        className="icon-button applet-actions-button"
        disabled={disabled}
        aria-label={`Actions for ${applet.name}`}
        aria-haspopup="dialog"
        onClick={(event) => showActions(event.currentTarget)}
      >
        <Icon name="more" />
      </button>
    </div>
  );
}
