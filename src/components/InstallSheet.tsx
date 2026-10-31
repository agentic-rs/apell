import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './Icon';

interface InstallSheetProps {
  open: boolean;
  busy: boolean;
  hasSample: boolean;
  status: string;
  failed: boolean;
  onClose: () => void;
  onSample: () => void;
  onFile: (file: File) => void;
  onUrl: (url: string, sha256: string) => void;
}

export function InstallSheet({
  open,
  busy,
  hasSample,
  status,
  failed,
  onClose,
  onSample,
  onFile,
  onUrl,
}: InstallSheetProps) {
  const panel = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  const [url, setUrl] = useState('');
  const [sha256, setSha256] = useState('');
  useEffect(() => {
    if (!open || !panel.current) return;
    const element = panel.current;
    const previous_focus = document.activeElement;
    const root = document.getElementById('root');
    const previous_hidden = root?.getAttribute('aria-hidden');
    const scroll_x = window.scrollX;
    const scroll_y = window.scrollY;
    const body = document.body;
    const previous_styles = {
      overflow: body.style.overflow,
      position: body.style.position,
      top: body.style.top,
      left: body.style.left,
      width: body.style.width,
    };
    const focusable = () =>
      [
        ...element.querySelectorAll<HTMLElement>(
          'a[href], button, input, select, textarea, summary, [tabindex]',
        ),
      ].filter(
        (candidate) =>
          candidate.tabIndex >= 0 &&
          !candidate.matches(':disabled') &&
          candidate.getClientRects().length > 0,
      );
    const focusFirst = () => (focusable()[0] ?? element).focus({ preventScroll: true });
    let reveal_frame: number | undefined;
    const revealFocus = () => {
      if (reveal_frame !== undefined) cancelAnimationFrame(reveal_frame);
      // Viewport listeners update the sheet bounds first; measure after layout.
      reveal_frame = requestAnimationFrame(() => {
        reveal_frame = undefined;
        const focused = document.activeElement;
        if (
          !(focused instanceof HTMLElement) ||
          !element.contains(focused) ||
          focused === element
        ) {
          return;
        }
        const panel_bounds = element.getBoundingClientRect();
        const focus_bounds = focused.getBoundingClientRect();
        const top = panel_bounds.top + element.clientTop + 12;
        const bottom = panel_bounds.top + element.clientTop + element.clientHeight - 12;
        if (focus_bounds.top < top || focus_bounds.height > bottom - top) {
          element.scrollTop += focus_bounds.top - top;
        } else if (focus_bounds.bottom > bottom) {
          element.scrollTop += focus_bounds.bottom - bottom;
        }
      });
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.isComposing) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        close.current();
      } else if (event.key === 'Tab') {
        event.preventDefault();
        const candidates = focusable();
        const current = candidates.indexOf(document.activeElement as HTMLElement);
        const next = event.shiftKey
          ? current <= 0
            ? candidates.length - 1
            : current - 1
          : current + 1 >= candidates.length
            ? 0
            : current + 1;
        (candidates[next] ?? element).focus();
      }
    };
    const onFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !element.contains(event.target)) focusFirst();
      else revealFocus();
    };
    Object.assign(body.style, {
      overflow: 'hidden',
      position: 'fixed',
      top: `${-scroll_y}px`,
      left: `${-scroll_x}px`,
      width: '100%',
    });
    focusFirst();
    root?.setAttribute('aria-hidden', 'true');
    document.addEventListener('keydown', onKeyDown, true);
    document.addEventListener('focusin', onFocus);
    window.visualViewport?.addEventListener('resize', revealFocus);
    window.addEventListener('resize', revealFocus);
    revealFocus();
    return () => {
      document.removeEventListener('keydown', onKeyDown, true);
      document.removeEventListener('focusin', onFocus);
      window.visualViewport?.removeEventListener('resize', revealFocus);
      window.removeEventListener('resize', revealFocus);
      if (reveal_frame !== undefined) cancelAnimationFrame(reveal_frame);
      if (previous_hidden == null) root?.removeAttribute('aria-hidden');
      else root?.setAttribute('aria-hidden', previous_hidden);
      Object.assign(body.style, previous_styles);
      window.scrollTo(scroll_x, scroll_y);
      if (previous_focus instanceof HTMLElement && previous_focus.isConnected) {
        previous_focus.focus({ preventScroll: true });
      }
    };
  }, [open]);

  if (!open) return null;
  return createPortal(
    <div
      className="sheet-layer"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={panel}
        className="install-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="install-title"
        tabIndex={-1}
      >
        <div className="sheet-content">
          <header className="sheet-header">
            <h2 id="install-title">Add an applet</h2>
            <button className="icon-button" aria-label="Close" onClick={onClose}>
              <Icon name="close" />
            </button>
          </header>
          <button className="install-option" disabled={busy} onClick={onSample}>
            <span className="option-icon">
              <Icon name="chat" />
            </span>
            <span>
              <strong>{hasSample ? 'Update Companion' : 'Try Companion'}</strong>
              <small>
                {hasSample ? 'Refresh the sample app and keep your chats' : 'A sample chatbot'}
              </small>
            </span>
            <Icon name="add" />
          </button>
          <label className={`install-option file-option${busy ? ' disabled' : ''}`}>
            <span className="option-icon">
              <Icon name="file" />
            </span>
            <span>
              <strong>Choose a bundle</strong>
              <small>From Files on this device</small>
            </span>
            <input
              type="file"
              accept=".json,.apell"
              aria-label="Choose a bundle"
              disabled={busy}
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = '';
                if (file) onFile(file);
              }}
            />
          </label>
          <details className="url-install">
            <summary>Install from a URL</summary>
            <form
              onSubmit={(event) => {
                event.preventDefault();
                onUrl(url.trim(), sha256.trim());
              }}
            >
              <label>
                Bundle URL
                <input
                  type="url"
                  required
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder="https://example.com/app.apell.json"
                  value={url}
                  disabled={busy}
                  onChange={(event) => setUrl(event.target.value)}
                />
              </label>
              <label>
                SHA-256 checksum
                <input
                  required
                  pattern="[a-fA-F0-9]{64}"
                  autoCapitalize="none"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder="64-character checksum"
                  value={sha256}
                  disabled={busy}
                  onChange={(event) => setSha256(event.target.value)}
                />
              </label>
              <button className="primary" disabled={busy}>
                {busy ? 'Installing…' : 'Download & install'}
              </button>
            </form>
          </details>
          {status && (
            <p className={`status${failed ? ' error' : ''}`} role={failed ? 'alert' : 'status'}>
              {status}
            </p>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
