import { useLayoutEffect, useRef, useState } from 'react';
import { attachBridge } from '../lib/bridge';
import { callApplet, onAppletEvent, onBackendEvent, message } from '../lib/tauri';
import type { AppletSession } from '../lib/types';
import { AppletControls } from '../components/AppletControls';
import { LlmModelPicker } from '../components/LlmModelPicker';
import { AppletModelChooser, type ModelChoice } from '../lib/modelChoice';
import { useBackAction, useBackNavigation } from '../lib/backNavigation';
import { AppletBack } from '../lib/appletBack';

const BACK_PRIORITY = 50;
type ViewAction = 'close' | 'reload' | 'restart';

export function AppletView({
  session,
  onClose,
  onReload,
  onRestart,
}: {
  session: AppletSession;
  onClose: () => Promise<void>;
  onReload: () => Promise<void>;
  onRestart: () => Promise<void>;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState('');
  const [suspended, setSuspended] = useState(false);
  const [stopped, setStopped] = useState(false);
  const [action, setAction] = useState<ViewAction | null>(null);
  const [choice, setChoice] = useState<ModelChoice>();
  const [installing_channel, setInstallingChannel] = useState(false);
  const visible_choice = installing_channel ? undefined : choice;
  const restart_failed = useRef(false);
  const stopBridge = useRef<(() => void) | undefined>(undefined);
  const guestBack = useRef<((close: () => void) => void) | undefined>(undefined);
  const navigation = useBackNavigation();
  const pending = useRef(false);
  useLayoutEffect(() => {
    if (suspended) return;
    let dispose: (() => void) | undefined;
    let unlisten: (() => void) | undefined;
    let unlisten_backend: (() => void) | undefined;
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
    const load_error = 'The applet did not finish loading. Use the menu to reload it.';
    const timeout = setTimeout(() => {
      if (!cancelled) setError(load_error);
    }, 15_000);
    const onLoad = () => {
      if (cancelled || element?.getAttribute('src') !== session.entry_url) return;
      clearTimeout(timeout);
      setError((current) => (current === load_error ? '' : current));
      setReady(true);
    };
    const initialize = async (supports_back: boolean) => {
      if (initializing || cancelled) return;
      initializing = true;
      const back = supports_back ? new AppletBack(channel.port1) : undefined;
      if (back) {
        // Reserve Back as soon as the document advertises support. An input
        // during bridge attachment should not be mistaken for the applet root.
        guestBack.current = () => {};
      }
      try {
        unlisten = await onAppletEvent((event) => {
          if (event.session_id === session.view_id && !cancelled)
            channel.port1.postMessage({ type: 'event', event: event.event });
        });
        if (cancelled) {
          unlisten();
          return;
        }
        unlisten_backend = await onBackendEvent((event) => {
          if (event.session_id === session.session_id && !cancelled) {
            if (event.event.name === 'apell:stopped') {
              setStopped(true);
              setSuspended(true);
              setError(
                'The applet backend stopped. Close and reopen the applet to start it again.',
              );
              return;
            }
            channel.port1.postMessage({ type: 'backend_event', event: event.event });
          }
        });
        if (cancelled) {
          unlisten_backend();
          return;
        }
        // Subscribe before validating: a backend can stop while its document
        // loads. The check also detects a stop that preceded subscription.
        await callApplet(session.session_id, session.view_id, 'view.check', {});
        if (cancelled) return;
        dispose = attachBridge(
          channel.port1,
          (method, params) => {
            if (method === 'llm.choose_model') {
              if (!session.manifest.network)
                return Promise.reject(new Error('This applet has disabled network access.'));
              return chooser.open(params);
            }
            return callApplet(session.session_id, session.view_id, method, params);
          },
          back,
        );
        element?.contentWindow?.postMessage({ type: 'apell:init' }, '*', [channel.port2]);
        if (back) guestBack.current = (close) => back.request(close);
        clearTimeout(timeout);
        setError('');
        setReady(true);
      } catch (error) {
        clearTimeout(timeout);
        if (!cancelled) {
          setError(`Could not attach to the applet. Close and reopen it. ${message(error)}`);
          setStopped(true);
          setSuspended(true);
        }
      }
    };
    const onReady = (event: MessageEvent<unknown>) => {
      if (event.source !== element?.contentWindow || !event.data || typeof event.data !== 'object')
        return;
      const { type, navigation_back } = event.data as Record<string, unknown>;
      if (type === 'apell:ready') {
        if (initializing) void run('reload');
        else void initialize(navigation_back === true);
      }
    };
    window.addEventListener('message', onReady);
    // The iframe has its entry src at insertion; we never mount a separate
    // about:blank iframe and then count its load as the package document.
    // Static pages only signal load. SDK pages also establish a port.
    element?.addEventListener('load', onLoad);
    const stop = () => {
      if (cancelled) return;
      cancelled = true;
      guestBack.current = undefined;
      clearTimeout(timeout);
      window.removeEventListener('message', onReady);
      element?.removeEventListener('load', onLoad);
      dispose?.();
      chooser.close();
      unlisten?.();
      unlisten_backend?.();
      channel.port1.close();
      channel.port2.close();
    };
    stopBridge.current = stop;
    return () => {
      stop();
      if (stopBridge.current === stop) stopBridge.current = undefined;
    };
  }, [session, suspended]);

  const run = async (next_action: ViewAction) => {
    if (pending.current || installing_channel) return;
    pending.current = true;
    stopBridge.current?.();
    setChoice(undefined);
    // Revoke the old bridge before requesting a new view of the running applet.
    setSuspended(true);
    setAction(next_action);
    setError('');
    try {
      await { close: onClose, reload: onReload, restart: onRestart }[next_action]();
    } catch (error) {
      restart_failed.current = next_action === 'restart';
      setError(`Could not ${next_action} the applet. ${message(error)}`);
    } finally {
      pending.current = false;
      setAction(null);
    }
  };

  // Sheets handle Back first. The applet then gets a chance to go back
  // internally; only its root falls through to the existing scoped close.
  useBackAction(BACK_PRIORITY, true, () => {
    if (choice || installing_channel || pending.current) return;
    const close = () => {
      if (!navigation?.hasHigherPriority(BACK_PRIORITY)) void run('close');
    };
    if (guestBack.current) guestBack.current(close);
    else close();
  });

  return (
    <section className="applet-view">
      <header className="applet-bar">
        <span>{session.manifest.name}</span>
        <AppletControls
          key={visible_choice ? 'choosing-model' : 'applet-controls'}
          session={session}
          disabled={action !== null || !!visible_choice || installing_channel}
          stopped={stopped}
          installing_channel={installing_channel}
          onInstallingChannel={setInstallingChannel}
          onClose={() => void run('close')}
          onReload={() => void run('reload')}
          onRestart={() => void run('restart')}
        />
      </header>
      {suspended ? (
        <div className="applet-state">
          <p className={error ? 'status error' : 'status'} role={error ? 'alert' : 'status'}>
            {error ||
              (action === 'close'
                ? 'Closing applet…'
                : action === 'restart'
                  ? 'Restarting applet…'
                  : 'Reloading applet…')}
          </p>
          {!action && (
            <button
              className="primary"
              onClick={() =>
                void run(restart_failed.current ? 'restart' : stopped ? 'close' : 'reload')
              }
            >
              {restart_failed.current
                ? 'Try restarting'
                : stopped
                  ? 'Close applet'
                  : 'Try reloading'}
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
            src={session.entry_url}
            title={session.manifest.name}
            sandbox="allow-scripts"
            allow="microphone 'none'; camera 'none'; geolocation 'none'"
            referrerPolicy="no-referrer"
          />
        </>
      )}
      {visible_choice && !suspended && (
        <LlmModelPicker key={visible_choice.id} choice={visible_choice} />
      )}
    </section>
  );
}
