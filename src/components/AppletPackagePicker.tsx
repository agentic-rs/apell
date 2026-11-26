import { Icon } from './Icon';

export function AppletPackagePicker({
  title,
  description,
  label,
  disabled,
  onFile,
}: {
  title: string;
  description?: string;
  label: string;
  disabled: boolean;
  onFile: (file: File) => void;
}) {
  return (
    <label className={`install-option file-option${disabled ? ' disabled' : ''}`}>
      <span className="option-icon">
        <Icon name="file" />
      </span>
      <span>
        <strong>{title}</strong>
        {description && <small>{description}</small>}
      </span>
      <input
        type="file"
        accept=".apel"
        aria-label={label}
        disabled={disabled}
        onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = '';
          if (file) onFile(file);
        }}
      />
    </label>
  );
}
