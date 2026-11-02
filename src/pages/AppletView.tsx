import { useEffect, useRef, useState } from 'react';
import { attachBridge } from '../lib/bridge';
import { callApplet, onAppletEvent, message } from '../lib/tauri';
import type { AppletSession } from '../lib/types';
import { AppletControls } from '../components/AppletControls';
import { LlmModelPicker } from '../components/LlmModelPicker';
import { AppletModelChooser, type ModelChoice } from '../lib/modelChoice';

export function AppletView({
  session,
  onClose,
  onReload,
}: {
  session: AppletSession;
  onClose: () => Promise<void>;
  onReload: () => Promise<void>;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  const [suspended, setSuspended] = useState(false);
  const [action, setAction] = useState<'close' | 'reload' | null>(null);
  const [choice, setChoice] = useState<ModelChoice>();
  const stopBridge = useRef<(() => void) | undefined>(undefined);
  const pending = useRef(false);
  useEffect(() => {
    if (suspended) return;
    let dispose: (() => void) | undefined;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const chooser = new AppletModelChooser(session.manifest.permissions, (next) => {
      if (!cancelled) setChoice(next);
    });
    let initializing = false;
    const channel = new MessageChannel();
    const element = frame.current;
    setChoice(undefined);
    setReady(false);
    setError('');
    const timeout = setTimeout(() => {
      if (!cancelled) setError('The applet did not connect. Use the menu to reload it.');
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
        dispose = attachBridge(channel.port1, (method, params) => {
          if (method === 'llm.choose_model') return chooser.open(params);
          return callApplet(session.session_id, method, params);
        });
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
    const stop = () => {
      if (cancelled) return;
      cancelled = true;
      clearTimeout(timeout);
      window.removeEventListener('message', onReady);
      dispose?.();
      chooser.close();
      unlisten?.();
      channel.port1.close();
      channel.port2.close();
    };
    stopBridge.current = stop;
    return () => {
      stop();
      if (stopBridge.current === stop) stopBridge.current = undefined;
    };
  }, [session, suspended]);

  const run = async (next_action: 'close' | 'reload') => {
    if (pending.current) return;
    pending.current = true;
    stopBridge.current?.();
    setChoice(undefined);
    // Suspend the old iframe and bridge while changing its native session.
    // On failure it stays suspended; a retry must create a fresh session.
    setSuspended(true);
    setAction(next_action);
    setError('');
    try {
      await (next_action === 'reload' ? onReload() : onClose());
    } catch (error) {
      setError(`Could not ${next_action} the applet. ${message(error)}`);
    } finally {
      pending.current = false;
      setAction(null);
    }
  };

  return (
    <section className="applet-view">
      <header className="applet-bar">
        <span>{session.manifest.name}</span>
        <AppletControls
          key={choice ? 'choosing-model' : 'applet-controls'}
          manifest={session.manifest}
          disabled={action !== null || !!choice}
          onClose={() => void run('close')}
          onReload={() => void run('reload')}
        />
      </header>
      {suspended ? (
        <div className="applet-state">
          <p className={error ? 'status error' : 'status'} role={error ? 'alert' : 'status'}>
            {error || (action === 'close' ? 'Closing applet…' : 'Reloading applet…')}
          </p>
          {!action && (
            <button className="primary" onClick={() => void run('reload')}>
              Try reloading
            </button>
          )}
        </div>
      ) : (
        <>
          {(!ready || error) && (
            <div
              className={`applet-loading${error ? ' error' : ''}`}
              role={error ? 'alert' : 'status'}
            >
              {error || 'Opening applet…'}
            </div>
          )}
          <iframe
            ref={frame}
            title={session.manifest.name}
            sandbox="allow-scripts"
            referrerPolicy="no-referrer"
          />
        </>
      )}
      {choice && !suspended && <LlmModelPicker key={choice.id} choice={choice} />}
    </section>
  );
}
