import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { listRunningApplets, message, onAppletRunsChanged, stopRunningApplet } from '../lib/tauri';
import type { AppletRun } from '../lib/types';
import { Icon } from './Icon';
import { Sheet } from './Sheet';
import './RunningApplets.css';

export function RunningApplets({
  disabled,
  onBusyChange,
  onStopped,
  onReturnToLibrary,
}: {
  disabled: boolean;
  onBusyChange: (busy: boolean) => void;
  onStopped: (run: AppletRun) => void;
  onReturnToLibrary: () => void;
}) {
  const [runs, setRuns] = useState<AppletRun[]>();
  const [load_error, setLoadError] = useState('');
  const [checking, setChecking] = useState(true);
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState<AppletRun>();
  const [stopping, setStopping] = useState(false);
  const [stop_error, setStopError] = useState('');
  const [notice, setNotice] = useState('');
  const revision = useRef(0);
  const pending = useRef(false);
  const shortcut = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const keep = useRef<HTMLButtonElement>(null);
  const focus_list = useRef<{ session_id?: string }>(undefined);
  const restore_shortcut = useRef(false);

  const refresh = useCallback(async () => {
    const request = ++revision.current;
    setChecking(true);
    try {
      const next = await listRunningApplets();
      if (revision.current === request) {
        setRuns(next);
        setLoadError('');
      }
    } catch (error) {
      if (revision.current === request) setLoadError(message(error));
    } finally {
      if (revision.current === request) setChecking(false);
    }
  }, []);

  useEffect(() => {
    let active = true;
    const update = () => {
      if (active) void refresh();
    };
    const subscription = onAppletRunsChanged(update).catch(() => undefined);
    void subscription.then(update);
    return () => {
      active = false;
      revision.current++;
      void subscription.then((unlisten) => unlisten?.());
    };
  }, [refresh]);

  useLayoutEffect(() => {
    if (target) {
      keep.current?.focus({ preventScroll: true });
    } else if (focus_list.current) {
      const { session_id } = focus_list.current;
      focus_list.current = undefined;
      const button = session_id ? document.getElementById(`stop-run-${session_id}`) : null;
      const focus = button instanceof HTMLButtonElement && !button.disabled ? button : list.current;
      focus?.focus({ preventScroll: true });
    }
  }, [target]);

  useEffect(() => {
    if (open || !restore_shortcut.current) return;
    restore_shortcut.current = false;
    if (shortcut.current) shortcut.current.focus({ preventScroll: true });
    else onReturnToLibrary();
  }, [open, onReturnToLibrary]);

  const count = runs?.length ?? 0;
  const exact_run =
    target &&
    runs?.some((run) => run.session_id === target.session_id && run.applet_id === target.applet_id);

  const close = () => {
    if (pending.current) return;
    if (target) {
      focus_list.current = { session_id: target.session_id };
      setTarget(undefined);
    } else {
      restore_shortcut.current = true;
      setOpen(false);
    }
    setStopError('');
  };
  const stop = async () => {
    if (!target || !exact_run || load_error || pending.current) return;
    pending.current = true;
    setStopping(true);
    setStopError('');
    onBusyChange(true);
    try {
      await stopRunningApplet(target);
      revision.current++;
      setRuns((current) => current?.filter((run) => run.session_id !== target.session_id));
      onStopped(target);
      setNotice(`${target.name} stopped.`);
      focus_list.current = {};
      setTarget(undefined);
    } catch (error) {
      setStopError(message(error));
    } finally {
      await refresh();
      pending.current = false;
      setStopping(false);
      onBusyChange(false);
    }
  };

  return (
    <>
      {(count > 0 || load_error) && (
        <button
          type="button"
          ref={shortcut}
          className="library-shortcut library-running-shortcut"
          disabled={disabled}
          aria-haspopup="dialog"
          onClick={(event) => {
            event.currentTarget.focus({ preventScroll: true });
            setNotice('');
            setOpen(true);
            void refresh();
          }}
        >
          <Icon name="activity" />
          <span>
            {count ? `${count} running ${count === 1 ? 'applet' : 'applets'}` : 'Running applets'}
          </span>
          <span className="library-shortcut-arrow">
            <Icon name="back" />
          </span>
        </button>
      )}
      <Sheet
        open={open}
        title={target ? `Stop ${target.name}?` : 'Running applets'}
        dismissible={!stopping}
        onClose={close}
      >
        {target ? (
          <>
            <p>Work still running will end. Saved data stays on this device.</p>
            {(stop_error || load_error || (!exact_run && !stopping)) && (
              <p className="status error" role="alert">
                {stop_error ||
                  load_error ||
                  'This run has already ended. Go back to review the current list.'}
              </p>
            )}
            <div className="button-row">
              <button ref={keep} type="button" disabled={stopping} onClick={close}>
                Keep running
              </button>
              <button
                type="button"
                className="danger"
                disabled={stopping || checking || !!load_error || !exact_run}
                onClick={() => void stop()}
              >
                {stopping ? 'Stopping…' : 'Stop applet'}
              </button>
            </div>
          </>
        ) : (
          <div
            ref={list}
            className="running-applet-list"
            tabIndex={-1}
            aria-busy={checking || stopping}
          >
            {load_error && (
              <div>
                <p className="status error" role="alert">
                  Couldn’t check running applets. {load_error}
                </p>
                <button
                  type="button"
                  disabled={checking || stopping}
                  onClick={() => void refresh()}
                >
                  Try again
                </button>
              </div>
            )}
            {count > 0 ? (
              <ul>
                {runs?.map((run) => {
                  const duplicate = runs.some((other) => other !== run && other.name === run.name);
                  return (
                    <li key={run.session_id}>
                      <div className="running-applet-name">
                        <strong>
                          <bdi>{run.name}</bdi>
                        </strong>
                        <small>
                          Version {run.version}
                          {duplicate && (
                            <>
                              {' '}
                              · <bdi>{run.applet_id}</bdi>
                            </>
                          )}
                        </small>
                      </div>
                      <button
                        id={`stop-run-${run.session_id}`}
                        type="button"
                        disabled={checking || stopping || !!load_error}
                        aria-label={`Stop ${run.name}${duplicate ? ` (${run.applet_id})` : ''}`}
                        onClick={() => {
                          setTarget(run);
                          setNotice('');
                        }}
                      >
                        Stop
                      </button>
                    </li>
                  );
                })}
              </ul>
            ) : (
              !load_error && (
                <p className="status" role="status">
                  {checking && !runs ? 'Checking running applets…' : 'No applets are running.'}
                </p>
              )
            )}
            {notice && (
              <p className="status" role="status">
                {notice}
              </p>
            )}
          </div>
        )}
      </Sheet>
    </>
  );
}
