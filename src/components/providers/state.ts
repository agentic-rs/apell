import type {
  LlmConnection,
  LlmModel,
  LlmProvider,
  LlmSettings,
  LoginChallenge,
  SaveConnection,
} from '../../lib/types';

export interface ConnectionDraft {
  name: string;
  provider_id: string;
  base_url: string;
  model_id: string;
  key_action: SaveConnection['key_action'];
  api_key: string;
}

export function connectionDraft(
  connection: LlmConnection | undefined,
  providers: LlmProvider[],
): ConnectionDraft {
  const provider =
    providers.find((item) => item.provider_id === 'openai') ??
    providers.find((item) => item.provider_id !== 'demo') ??
    providers[0];
  return {
    name: connection?.name ?? provider?.name ?? '',
    provider_id: connection?.provider_id ?? provider?.provider_id ?? '',
    base_url: connection?.base_url ?? provider?.base_url ?? '',
    model_id: connection?.model_id ?? '',
    key_action:
      !connection &&
      provider?.auth_methods.includes('api_key') &&
      !provider.auth_methods.includes('none')
        ? 'replace'
        : 'keep',
    api_key: '',
  };
}

export function changeProvider(draft: ConnectionDraft, provider: LlmProvider): ConnectionDraft {
  return {
    ...draft,
    provider_id: provider.provider_id,
    base_url: provider.base_url,
    model_id: provider.provider_id === 'demo' ? 'local-demo' : '',
    key_action:
      provider.auth_methods.includes('api_key') && !provider.auth_methods.includes('none')
        ? 'replace'
        : 'remove',
    api_key: '',
  };
}

function normalizedEndpoint(value: string): string {
  return value.trim().replace(/\/+$/, '');
}

export function destinationChanged(draft: ConnectionDraft, saved?: LlmConnection): boolean {
  return (
    !!saved &&
    (draft.provider_id !== saved.provider_id ||
      normalizedEndpoint(draft.base_url) !== normalizedEndpoint(saved.base_url))
  );
}

export function savePayload(draft: ConnectionDraft, saved?: LlmConnection): SaveConnection {
  const key_action =
    destinationChanged(draft, saved) && draft.key_action === 'keep' ? 'remove' : draft.key_action;
  return {
    ...(saved ? { connection_id: saved.connection_id } : {}),
    name: draft.name.trim(),
    provider_id: draft.provider_id,
    base_url: normalizedEndpoint(draft.base_url),
    model_id: draft.model_id.trim(),
    key_action,
    ...(key_action === 'replace' ? { api_key: draft.api_key } : {}),
  };
}

export function hasChanges(draft: ConnectionDraft, saved?: LlmConnection): boolean {
  if (!saved) return true;
  const payload = savePayload(draft, saved);
  return (
    payload.name !== saved.name ||
    payload.provider_id !== saved.provider_id ||
    payload.base_url !== normalizedEndpoint(saved.base_url) ||
    payload.model_id !== saved.model_id ||
    payload.key_action !== 'keep'
  );
}

export function matchingModels(models: LlmModel[], query: string): LlmModel[] {
  const search = query.trim().toLocaleLowerCase();
  return models.filter((model) =>
    `${model.model_id} ${model.name}`.toLocaleLowerCase().includes(search),
  );
}

export function requestId(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
    byte.toString(16).padStart(2, '0'),
  ).join('');
}

export async function stopLogin(
  begin: Promise<LoginChallenge> | undefined,
  cancel: (login_id: string) => Promise<void>,
  refresh: () => Promise<LlmSettings>,
): Promise<LlmSettings> {
  const challenge = await begin?.catch(() => undefined);
  if (challenge) await cancel(challenge.login_id);
  return refresh();
}

/** One foreground operation; late responses cannot update a different editor. */
export class OperationScope {
  private current?: Operation;

  start(): Operation | undefined {
    if (this.current) return undefined;
    const operation: Operation = new Operation(
      () => this.current === operation,
      () => {
        if (this.current === operation) this.current = undefined;
      },
    );
    this.current = operation;
    return operation;
  }

  cancel(): void {
    const operation = this.current;
    this.current = undefined;
    operation?.cancel();
  }
}

export class Operation {
  private cancellation?: () => void;

  constructor(
    readonly isCurrent: () => boolean,
    private readonly release: () => void,
  ) {}

  setCancellation(cancel: () => void): void {
    if (this.isCurrent()) this.cancellation = cancel;
    else cancel();
  }

  cancel(): void {
    this.release();
    const cancel = this.cancellation;
    this.cancellation = undefined;
    cancel?.();
  }

  finish(): void {
    this.cancellation = undefined;
    this.release();
  }
}
