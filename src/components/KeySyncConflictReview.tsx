import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { useBackAction } from '../lib/backNavigation';
import { resolveKeySyncConflict, reviewKeySyncConflicts } from '../lib/tauri';
import type { KeySyncConflict, KeySyncConflictResolution, LlmProvider } from '../lib/types';
import { OperationScope } from './providers/state';

type Choice = { conflict: KeySyncConflict; resolution: KeySyncConflictResolution['resolution'] };
type Review =
  | { state: 'loading' }
  | { state: 'ready'; conflicts: KeySyncConflict[]; updated: boolean }
  | { state: 'stale'; error: string };

export function keyConflictConfirmation({ conflict, resolution }: Choice) {
  if (resolution === 'keep_device') {
    if (!conflict.device_has_key) {
      return {
        description: 'This device has no saved key. Remove the saved key from your Account.',
        action: 'Remove cloud key',
        removes: true,
      };
    }
    return conflict.cloud_has_key
      ? {
          description: 'Replace the saved cloud key with the key on this device.',
          action: 'Replace cloud key',
          removes: false,
        }
      : {
          description: 'Save the key on this device to your Account.',
          action: 'Save to cloud',
          removes: false,
        };
  }
  if (!conflict.cloud_has_key) {
    return {
      description: 'The cloud has no saved key. Remove the saved key from this device.',
      action: 'Remove device key',
      removes: true,
    };
  }
  return conflict.device_has_key
    ? {
        description: 'Replace the saved key on this device with the cloud key.',
        action: 'Replace device key',
        removes: false,
      }
    : {
        description: 'Save the cloud key to this device.',
        action: 'Save to device',
        removes: false,
      };
}

export function KeySyncConflictReview({
  providers,
  disabled,
  onMutationChange,
  onApplied,
}: {
  providers: LlmProvider[];
  disabled: boolean;
  onMutationChange: (active: boolean) => void;
  onApplied: () => void;
}) {
  const [review, setReview] = useState<Review>({ state: 'loading' });
  const [choice, setChoice] = useState<Choice>();
  const [saving, setSaving] = useState(false);
  const mutation_pending = useRef(false);
  const scope = useRef(new OperationScope());
  const confirmation_id = useId();
  const cancel_button = useRef<HTMLButtonElement>(null);
  const choice_button = useRef<HTMLButtonElement>(null);

  useLayoutEffect(() => {
    if (choice) cancel_button.current?.focus({ preventScroll: true });
  }, [choice]);

  function choose(next: Choice, button: HTMLButtonElement) {
    choice_button.current = button;
    setChoice(next);
  }

  function cancelChoice() {
    if (mutation_pending.current) return;
    setChoice(undefined);
    const button = choice_button.current;
    choice_button.current = null;
    if (button?.isConnected && !button.disabled) button.focus({ preventScroll: true });
  }

  useBackAction(110, !!choice, cancelChoice);

  const loadReview = useCallback(async (updated = false) => {
    const operation = scope.current.start();
    if (!operation) return;
    setChoice(undefined);
    setReview({ state: 'loading' });
    try {
      const conflicts = await reviewKeySyncConflicts();
      if (operation.isCurrent()) setReview({ state: 'ready', conflicts, updated });
    } catch {
      if (operation.isCurrent()) {
        setReview({ state: 'stale', error: 'Couldn’t check key conflicts. Refresh to try again.' });
      }
    } finally {
      operation.finish();
    }
  }, []);

  useEffect(() => {
    void loadReview();
    return () => scope.current.cancel();
  }, [loadReview]);

  async function applyChoice() {
    if (!choice || review.state !== 'ready' || disabled || saving) return;
    const operation = scope.current.start();
    if (!operation) return;
    mutation_pending.current = true;
    setSaving(true);
    onMutationChange(true);
    let updated = false;
    try {
      const { warning } = await resolveKeySyncConflict({
        connection_id: choice.conflict.connection_id,
        review_id: choice.conflict.review_id,
        resolution: choice.resolution,
      });
      if (!operation.isCurrent()) return;
      setChoice(undefined);
      if (warning) {
        setReview({ state: 'stale', error: warning });
      } else {
        updated = true;
      }
    } catch {
      if (operation.isCurrent()) {
        setChoice(undefined);
        setReview({
          state: 'stale',
          error: 'The change couldn’t be confirmed. Refresh the review before choosing again.',
        });
      }
    } finally {
      mutation_pending.current = false;
      if (operation.isCurrent()) setSaving(false);
      operation.finish();
      onMutationChange(false);
      // A cloud write may have succeeded even when the complete response was lost.
      onApplied();
    }
    if (updated) void loadReview(true);
  }

  const confirmation = choice && keyConflictConfirmation(choice);
  const progress = saving
    ? 'Applying key choice…'
    : review.state === 'loading'
      ? 'Checking key conflicts…'
      : review.state === 'ready' && !review.conflicts.length
        ? 'No key conflicts to review.'
        : review.state === 'ready' && review.updated
          ? 'Key conflict resolved.'
          : '';
  const actions_disabled = disabled || saving;

  return (
    <div className="transfer-form key-conflict-review">
      <p className="key-conflict-progress status" role="status" aria-live="polite">
        {progress}
      </p>
      {review.state === 'ready' && review.conflicts.length > 0 && (
        <ul className="sync-review-changes" aria-busy={saving}>
          {review.conflicts.map((conflict) => {
            const provider = providers.find((item) => item.provider_id === conflict.provider_id);
            const label_is_ambiguous = review.conflicts.some(
              (other) =>
                other.connection_id !== conflict.connection_id &&
                other.name === conflict.name &&
                other.provider_id === conflict.provider_id &&
                other.model_id === conflict.model_id,
            );
            const selected = choice?.conflict === conflict;
            const label = label_is_ambiguous
              ? `${conflict.name} (${conflict.connection_id})`
              : conflict.name;

            return (
              <li className="key-conflict-item" key={conflict.connection_id}>
                <div className="key-conflict-identity">
                  <h3>{conflict.name}</h3>
                  <small>
                    {provider?.name ?? conflict.provider_id} · {conflict.model_id}
                  </small>
                  {label_is_ambiguous && <small>Connection ID {conflict.connection_id}</small>}
                </div>
                <dl className="sync-review-pair key-conflict-sides">
                  <div>
                    <dt>This device</dt>
                    <dd>{conflict.device_has_key ? 'Key saved' : 'No saved key'}</dd>
                  </div>
                  <div>
                    <dt>Cloud</dt>
                    <dd>{conflict.cloud_has_key ? 'Key saved' : 'No saved key'}</dd>
                  </div>
                </dl>
                <div className="button-row">
                  <button
                    type="button"
                    aria-label={`Keep device for ${label}`}
                    aria-expanded={selected && choice.resolution === 'keep_device'}
                    disabled={actions_disabled}
                    onClick={(event) =>
                      choose({ conflict, resolution: 'keep_device' }, event.currentTarget)
                    }
                  >
                    Keep device
                  </button>
                  <button
                    type="button"
                    aria-label={`Use cloud for ${label}`}
                    aria-expanded={selected && choice.resolution === 'use_cloud'}
                    disabled={actions_disabled}
                    onClick={(event) =>
                      choose({ conflict, resolution: 'use_cloud' }, event.currentTarget)
                    }
                  >
                    Use cloud
                  </button>
                </div>
                {selected && confirmation && (
                  <div className="key-conflict-confirm">
                    <p id={confirmation_id}>
                      {confirmation.description}
                      {choice.resolution === 'keep_device' &&
                        ' Other devices can receive this change through sync.'}
                    </p>
                    <div className="button-row">
                      <button
                        ref={cancel_button}
                        type="button"
                        aria-describedby={confirmation_id}
                        disabled={saving}
                        onClick={cancelChoice}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        className={confirmation.removes ? 'danger' : 'primary'}
                        aria-describedby={confirmation_id}
                        disabled={actions_disabled}
                        onClick={() => void applyChoice()}
                      >
                        {confirmation.action}
                      </button>
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {review.state === 'stale' && (
        <>
          <p className="status error" role="alert">
            {review.error}
          </p>
          <button type="button" onClick={() => void loadReview()}>
            Refresh review
          </button>
        </>
      )}
    </div>
  );
}
