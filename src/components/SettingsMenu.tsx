import { useEffect, useState, type ComponentProps } from 'react';
import {
  getAccountStatus,
  getConfigSyncStatus,
  getLocalKeySyncStatus,
  onAccountChanged,
  onConfigSyncChanged,
  onKeySyncChanged,
} from '../lib/tauri';
import type {
  AccountStatus,
  ConfigSyncStatus,
  LlmSettings,
  LocalKeySyncStatus,
} from '../lib/types';
import { connectionReadiness, connectionReadinessLabel } from '../lib/modelChoice';
import { Icon } from './Icon';

export type SettingsSection =
  'account' | 'default-model' | 'connections' | 'permissions' | 'backup';

function SettingsRow({
  section,
  icon,
  title,
  detail,
  detail_note,
  attention,
  onOpen,
}: {
  section: SettingsSection;
  icon: ComponentProps<typeof Icon>['name'];
  title: string;
  detail: string;
  detail_note?: string;
  attention?: string;
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
        {detail_note && <small>{detail_note}</small>}
        {attention && <small className="settings-menu-attention">{attention}</small>}
      </span>
      <span className="settings-menu-chevron">
        <Icon name="back" />
      </span>
    </button>
  );
}

type AccountSummary = {
  detail: string;
  account_session_id?: string;
  verification?: 'Verified device' | 'Unverified device';
  attention?: string;
};

const checking_account: AccountSummary = { detail: 'Checking sign-in…' };

export function accountSyncAttention(model?: ConfigSyncStatus, keys?: LocalKeySyncStatus) {
  const model_sync = model?.state === 'ready' ? model : undefined;
  const key_sync = keys?.state === 'verified' ? keys.sync_state : undefined;
  if (model_sync?.pending_conflict_id) return 'Review model changes';
  if (key_sync === 'conflict') return 'Key sync conflict';
  if (model_sync?.sync_error && !model_sync.syncing) return 'Couldn’t sync model settings';
  switch (key_sync) {
    case 'failed':
      return 'Couldn’t sync keys';
    case 'paused':
      return 'Key sync paused';
    case 'waiting_for_model_settings':
      return 'Keys need model settings';
    default:
      return undefined;
  }
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
  const [account_summary, setAccountSummary] = useState(checking_account);
  useEffect(() => {
    let alive = true;
    let sequence = 0;
    const unlisten: (() => void)[] = [];
    const update = async () => {
      const request = ++sequence;
      const account = await getAccountStatus().catch(() => undefined);
      if (!alive || request !== sequence) return;
      const detail = account ? accountDetail(account) : 'Status unavailable';
      if (account?.state !== 'signed_in') {
        setAccountSummary({ detail });
        return;
      }
      const { account_session_id } = account;
      setAccountSummary((current) =>
        current.account_session_id === account_session_id
          ? { ...current, detail }
          : { detail, account_session_id },
      );
      const [model, keys] = await Promise.allSettled([
        getConfigSyncStatus(account_session_id),
        getLocalKeySyncStatus(account_session_id),
      ]);
      if (alive && request === sequence) {
        const key_status = keys.status === 'fulfilled' ? keys.value : undefined;
        const attention = accountSyncAttention(
          model.status === 'fulfilled' ? model.value : undefined,
          key_status,
        );
        const unavailable = model.status === 'rejected' || keys.status === 'rejected';
        setAccountSummary({
          detail,
          account_session_id,
          verification:
            key_status?.state === 'verified'
              ? 'Verified device'
              : key_status?.state === 'unverified'
                ? 'Unverified device'
                : undefined,
          attention: attention ?? (unavailable ? 'Sync status unavailable' : undefined),
        });
      }
    };
    const reload = () => {
      if (alive) void update();
    };
    const subscriptions = [
      onAccountChanged(() => {
        if (!alive) return;
        setAccountSummary(checking_account);
        reload();
      }),
      onConfigSyncChanged(reload),
      onKeySyncChanged(reload),
    ];
    window.addEventListener('focus', reload);
    reload();
    void Promise.allSettled(
      subscriptions.map((subscription) =>
        subscription.then((stop) => {
          if (alive) unlisten.push(stop);
          else stop();
        }),
      ),
    ).then(reload);
    return () => {
      alive = false;
      window.removeEventListener('focus', reload);
      for (const stop of unlisten) stop();
    };
  }, []);

  const active = settings?.connections.find(
    (connection) => connection.connection_id === settings.active_connection_id,
  );
  const pending = settings?.connections.find(
    (connection) => connection.connection_id === settings.pending_preferred_connection_id,
  );
  const problem =
    active && settings
      ? connectionReadinessLabel(connectionReadiness(active, settings.providers))
      : undefined;
  const default_detail = active
    ? active.model_id && active.model_id !== active.name
      ? `${active.name} · ${active.model_id}`
      : active.name
    : settings
      ? 'No default selected'
      : loading
        ? 'Loading connections…'
        : 'Connections unavailable';
  const connection_detail = settings
    ? `${settings.connections.length} saved ${settings.connections.length === 1 ? 'connection' : 'connections'}`
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
          detail={account_summary.detail}
          detail_note={account_summary.verification}
          attention={account_summary.attention}
          onOpen={onOpen}
        />
      </div>
      <div className="settings-menu-group">
        <SettingsRow
          section="default-model"
          icon="chat"
          title="Default model"
          detail={default_detail}
          attention={problem ?? (pending ? 'Synced choice pending' : undefined)}
          onOpen={onOpen}
        />
        <SettingsRow
          section="connections"
          icon="connections"
          title="Model connections"
          detail={connection_detail}
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
