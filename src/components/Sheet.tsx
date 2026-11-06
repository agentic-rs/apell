import { useEffect, useId, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './Icon';

interface SheetProps {
  open: boolean;
  title: string;
  onClose: () => void;
  dismissible?: boolean;
  children: ReactNode;
}

export function Sheet({ open, title, onClose, dismissible = true, children }: SheetProps) {
  const title_id = useId();
  const panel = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = () => {
    if (dismissible) onClose();
  };
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
        if (event.target === event.currentTarget) close.current();
      }}
    >
      <div
        ref={panel}
        className="sheet-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby={title_id}
        tabIndex={-1}
      >
        <div className="sheet-content">
          <header className="sheet-header">
            <h2 id={title_id}>{title}</h2>
            <button
              className="icon-button"
              aria-label="Close"
              disabled={!dismissible}
              onClick={() => close.current()}
            >
              <Icon name="close" />
            </button>
          </header>
          {children}
        </div>
      </div>
    </div>,
    document.body,
  );
}
