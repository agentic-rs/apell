import type { StoreTransferControl } from './useStoreTransfer';

export function PauseStoreDownload({ control }: { control: StoreTransferControl }) {
  if (!control.started) return null;
  return (
    <div className="store-pause-download">
      <button type="button" disabled={!control.can_pause} onClick={control.pause}>
        {control.pausing ? 'Pausing…' : 'Pause download'}
      </button>
      {control.error && (
        <p className="status error" role="alert">
          {control.error}
        </p>
      )}
    </div>
  );
}
