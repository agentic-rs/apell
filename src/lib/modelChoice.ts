import type { LlmSelection, LlmTarget } from '../../packages/applet-sdk/src/types';
import type { LlmConnection, LlmProvider, LlmSettings } from './types';

export interface ModelChoice {
  id: number;
  current?: LlmTarget;
  finish: (selection: LlmSelection | null) => void;
}

function validModelId(model_id: unknown): model_id is string {
  return (
    typeof model_id === 'string' &&
    model_id.length <= 256 &&
    !!model_id.trim() &&
    new TextEncoder().encode(model_id).length <= 256 &&
    !/[\u0000-\u001f\u007f-\u009f]/u.test(model_id)
  );
}

function readCurrent(params: unknown): LlmTarget | undefined {
  const record = (value: unknown): value is Record<string, unknown> =>
    !!value &&
    typeof value === 'object' &&
    (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
  if (!record(params) || Object.keys(params).some((key) => key !== 'current')) {
    throw new Error('Invalid model picker options.');
  }
  if (!Object.hasOwn(params, 'current')) return undefined;
  const target = params.current;
  if (
    !record(target) ||
    Object.keys(target).some((key) => key !== 'connection_id' && key !== 'model_id') ||
    typeof target.connection_id !== 'string' ||
    !/^[a-zA-Z0-9_-]{1,128}$/.test(target.connection_id) ||
    !validModelId(target.model_id)
  )
    throw new Error('Invalid current model target.');
  return { connection_id: target.connection_id, model_id: target.model_id };
}

/** Each iframe session can have one visible request. An old sheet cannot settle a newer request. */
export class AppletModelChooser {
  private active?: ModelChoice;
  private next_id = 0;

  constructor(
    private readonly permissions: readonly string[],
    private readonly show: (choice?: ModelChoice) => void,
  ) {}

  async open(params: unknown): Promise<LlmSelection | null> {
    if (!this.permissions.includes('llm'))
      throw new Error('This applet did not declare the LLM permission.');
    if (this.active) throw new Error('The model picker is already open.');
    const current = readCurrent(params);
    return new Promise((resolve) => {
      const request: ModelChoice = {
        id: ++this.next_id,
        current,
        finish: (selection) => {
          if (this.active !== request) return;
          this.active = undefined;
          this.show();
          resolve(selection);
        },
      };
      this.active = request;
      this.show(request);
    });
  }

  close(): void {
    this.active?.finish(null);
  }
}

export type ConnectionReadiness =
  | { state: 'provider_unavailable'; provider?: undefined }
  | {
      state: 'ready' | 'needs_model' | 'needs_api_key' | 'needs_sign_in' | 'credential_unavailable';
      provider: LlmProvider;
    };

const connection_help = {
  needs_model: { label: 'Choose a model', action: 'Choose a model' },
  needs_api_key: { label: 'API key needed', action: 'Add API key' },
  needs_sign_in: { label: 'Provider sign-in needed', action: 'Sign in' },
  credential_unavailable: { label: 'Credential unavailable', action: 'Edit connection' },
  provider_unavailable: { label: 'Provider unavailable', action: 'Edit connection' },
};

export function connectionReadiness(
  connection: LlmConnection,
  providers: readonly LlmProvider[],
): ConnectionReadiness {
  const provider = providers.find((item) => item.provider_id === connection.provider_id);
  if (!provider) return { state: 'provider_unavailable' };
  if (connection.credential_status === 'unavailable') {
    return { state: 'credential_unavailable', provider };
  }
  if (connection.credential_status === 'absent' && !provider.auth_methods.includes('none')) {
    return {
      state: provider.auth_methods.includes('device_code') ? 'needs_sign_in' : 'needs_api_key',
      provider,
    };
  }
  return { state: validModelId(connection.model_id) ? 'ready' : 'needs_model', provider };
}

export function connectionReadinessLabel({ state }: ConnectionReadiness): string | undefined {
  return state === 'ready' ? undefined : connection_help[state].label;
}

export function connectionReadinessAction({ state }: ConnectionReadiness): string | undefined {
  return state === 'ready' ? undefined : connection_help[state].action;
}

export function selectableProvider(settings: LlmSettings, connection: LlmConnection) {
  const readiness = connectionReadiness(connection, settings.providers);
  return readiness.state === 'ready' || readiness.state === 'needs_model'
    ? readiness.provider
    : undefined;
}

export function initialModelTarget(
  settings: LlmSettings,
  current?: LlmTarget,
): LlmTarget | undefined {
  const { connections, providers, active_connection_id } = settings;
  if (current && connections.some((entry) => entry.connection_id === current.connection_id)) {
    return current;
  }
  const preferred = connections.find((entry) => entry.connection_id === active_connection_id);
  const candidates = preferred
    ? [preferred, ...connections.filter((entry) => entry !== preferred)]
    : connections;
  const connection =
    candidates.find((entry) => connectionReadiness(entry, providers).state === 'ready') ??
    candidates.find((entry) => connectionReadiness(entry, providers).state === 'needs_model') ??
    candidates[0];
  return connection && { connection_id: connection.connection_id, model_id: connection.model_id };
}

export function publicSelection(
  settings: LlmSettings,
  connection: LlmConnection,
  model_id: string,
): LlmSelection | undefined {
  const provider = selectableProvider(settings, connection);
  const selected = model_id.trim();
  if (!provider || !validModelId(selected)) return undefined;
  return {
    connection_id: connection.connection_id,
    model_id: selected,
    provider_id: provider.provider_id,
    provider_name: provider.name,
  };
}
