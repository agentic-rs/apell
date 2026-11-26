import { useEffect, useState, type ComponentProps } from 'react';
import { getAccountStatus, onAccountChanged } from '../lib/tauri';
import type { AccountStatus, LlmSettings } from '../lib/types';
import { Icon } from './Icon';

export type SettingsSection = 'account' | 'connections' | 'permissions' | 'backup';

function SettingsRow({
  section,
  icon,
  title,
  detail,
  onOpen,
}: {
  section: SettingsSection;
  icon: ComponentProps<typeof Icon>['name'];
  title: string;
  detail: string;
  onOpen: (section: SettingsSection) => void;
}) {
  return (
    <button
      type="button"
      id={`settings-open-${section}`}
      className="settings-menu-row"
      onClick={() => onOpen(section)}
    >
      <span className="settings-menu-icon">
        <Icon name={icon} />
      </span>
      <span className="settings-menu-text">
        <strong>{title}</strong>
        <small>{detail}</small>
      </span>
      <span className="settings-menu-chevron">
        <Icon name="back" />
      </span>
    </button>
  );
}

export function SettingsMenu({
  settings,
  loading,
  onOpen,
}: {
  settings?: LlmSettings;
  loading: boolean;
  onOpen: (section: SettingsSection) => void;
}) {
  const [account_detail, setAccountDetail] = useState('Checking sign-in…');
  useEffect(() => {
    let alive = true;
    let sequence = 0;
    let unsubscribe: (() => void) | undefined;
    const update = async () => {
      const request = ++sequence;
      const detail = await getAccountStatus().then(accountDetail, () => 'Status unavailable');
      if (alive && request === sequence) setAccountDetail(detail);
    };
    void update();
    void onAccountChanged(() => void update()).then(
      (stop) => {
        if (alive) {
          unsubscribe = stop;
          void update();
        } else stop();
      },
      () => {},
    );
    return () => {
      alive = false;
      unsubscribe?.();
    };
  }, []);

  const active_model = settings?.connections.find(
    (connection) => connection.connection_id === settings.active_connection_id,
  )?.model_id;
  const model_detail = settings
    ? active_model || 'No active model'
    : loading
      ? 'Loading connections…'
      : 'Connections unavailable';

  return (
    <nav className="panel settings-menu" aria-label="Settings sections">
      <div className="settings-menu-group settings-menu-account">
        <SettingsRow
          section="account"
          icon="account"
          title="Apell Account"
          detail={account_detail}
          onOpen={onOpen}
        />
      </div>
      <div className="settings-menu-group">
        <SettingsRow
          section="connections"
          icon="connections"
          title="Model connections"
          detail={model_detail}
          onOpen={onOpen}
        />
        <SettingsRow
          section="permissions"
          icon="shield"
          title="Applet permissions"
          detail="Manage access for installed applets"
          onOpen={onOpen}
        />
        <SettingsRow
          section="backup"
          icon="file"
          title="Transfer connections"
          detail="Import or export an encrypted backup"
          onOpen={onOpen}
        />
      </div>
    </nav>
  );
}

function accountDetail(account: AccountStatus): string {
  switch (account.state) {
    case 'signed_in':
      return account.email || 'Signed in';
    case 'signed_out':
      return 'Sign in to sync';
    case 'signing_in':
      return 'Sign-in in progress';
    case 'unavailable':
      return 'Sign-in unavailable';
  }
}
