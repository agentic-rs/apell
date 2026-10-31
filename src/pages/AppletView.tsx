import { useEffect, useRef, useState } from 'react';
import { attachBridge } from '../lib/bridge';
import { callApplet, onAppletEvent, message } from '../lib/tauri';
import type { AppletSession } from '../lib/types';
import { Icon } from '../components/Icon';

export function AppletView({
  session,
  onClose,
}: {
  session: AppletSession;
  onClose: () => Promise<void>;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  const [closing, setClosing] = useState(false);
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    let initializing = false;
    const channel = new MessageChannel();
    const element = frame.current;
    setReady(false);
    setError('');
    const timeout = setTimeout(() => {
      if (!cancelled) setError('The applet did not connect. Return to the library and reopen it.');
    }, 15_000);
    const initialize = async () => {
      if (initializing || cancelled) return;
      initializing = true;
      try {
        unlisten = await onAppletEvent((event) => {
          if (event.session_id === session.session_id && !cancelled)
            channel.port1.postMessage({ type: 'event', event: event.event });
        });
        if (cancelled) {
          unlisten();
          return;
        }
        dispose = attachBridge(channel.port1, (method, params) =>
          callApplet(session.session_id, method, params),
        );
        element?.contentWindow?.postMessage({ type: 'apell:init' }, '*', [channel.port2]);
        clearTimeout(timeout);
        setError('');
        setReady(true);
      } catch (error) {
        clearTimeout(timeout);
        if (!cancelled) setError(message(error));
      }
    };
    const onReady = (event: MessageEvent<unknown>) => {
      if (event.source !== element?.contentWindow || !event.data || typeof event.data !== 'object')
        return;
      if ((event.data as Record<string, unknown>).type === 'apell:ready') void initialize();
    };
    window.addEventListener('message', onReady);
    // The opaque iframe announces readiness after its SDK receiver is installed.
    if (element) element.srcdoc = session.html;
    return () => {
      cancelled = true;
      clearTimeout(timeout);
      window.removeEventListener('message', onReady);
      dispose?.();
      unlisten?.();
      channel.port1.close();
      channel.port2.close();
    };
  }, [session]);
  return (
    <section className="applet-view">
      <header className="applet-bar">
        <button
          className="icon-button"
          aria-label="Back to applets"
          disabled={closing}
          onClick={async () => {
            setClosing(true);
            try {
              await onClose();
            } catch (error) {
              setError(message(error));
              setClosing(false);
            }
          }}
        >
          <Icon name="back" />
        </button>
        <span>{session.manifest.name}</span>
      </header>
      {(!ready || error || closing) && (
        <div className={`applet-loading${error ? ' error' : ''}`} role={error ? 'alert' : 'status'}>
          {error || (closing ? 'Closing applet…' : 'Opening applet…')}
        </div>
      )}
      <iframe
        ref={frame}
        title={session.manifest.name}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
      />
    </section>
  );
}
