import type {
  CreateConnectionDraft,
  DraftCredential,
  LlmConnection,
  LlmModel,
  LlmProvider,
  NativeConnectionDraft,
  SaveConnection,
} from '../../lib/types';

export interface ConnectionDraft {
  name: string;
  provider_id: string;
  base_url: string;
  model_id: string;
  remove_credential: boolean;
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
    remove_credential: false,
    api_key: '',
  };
}

export function changeProvider(draft: ConnectionDraft, provider: LlmProvider): ConnectionDraft {
  return {
    ...draft,
    provider_id: provider.provider_id,
    base_url: provider.base_url,
    model_id: provider.provider_id === 'demo' ? 'local-demo' : '',
    remove_credential: false,
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

export function draftPayload(draft: ConnectionDraft, saved?: LlmConnection): CreateConnectionDraft {
  return {
    ...(saved ? { connection_id: saved.connection_id } : {}),
    provider_id: draft.provider_id,
    base_url: normalizedEndpoint(draft.base_url),
  };
}

export function credentialPayload(draft: ConnectionDraft): DraftCredential {
  const key_action = draft.remove_credential ? 'remove' : draft.api_key ? 'replace' : 'keep';
  return {
    key_action,
    ...(key_action === 'replace' ? { api_key: draft.api_key } : {}),
  };
}

export function savePayload(
  draft: ConnectionDraft,
  draft_id: string,
  provider_name: string,
): SaveConnection {
  const model_id = draft.model_id.trim();
  if (!model_id) throw new Error('Choose a model or enter its ID before saving.');
  return {
    draft_id,
    name: draft.name.trim() || provider_name,
    model_id,
    ...credentialPayload(draft),
  };
}

export function hasChanges(
  draft: ConnectionDraft,
  saved?: LlmConnection,
  staged_login = false,
): boolean {
  if (!saved) return true;
  return (
    staged_login ||
    draft.name.trim() !== saved.name ||
    destinationChanged(draft, saved) ||
    draft.model_id.trim() !== saved.model_id ||
    credentialPayload(draft).key_action !== 'keep'
  );
}

export function canSaveDraft(
  draft: ConnectionDraft,
  provider: LlmProvider | undefined,
  saved?: LlmConnection,
  staged_login = false,
): boolean {
  if (!provider || !draft.model_id.trim()) return false;
  if (saved && !destinationChanged(draft, saved)) return true;
  return (
    provider.auth_methods.includes('none') ||
    (!draft.remove_credential &&
      ((provider.auth_methods.includes('api_key') && !!draft.api_key) ||
        (provider.auth_methods.includes('device_code') && staged_login)))
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

interface DraftLease {
  key: string;
  promise: Promise<NativeConnectionDraft>;
  value?: NativeConnectionDraft;
  discarded: boolean;
  consumed: boolean;
}

/** Own temporary native auth until Save consumes it or this editor discards it. */
export class DraftSession {
  private current?: DraftLease;

  constructor(
    private readonly create: (input: CreateConnectionDraft) => Promise<NativeConnectionDraft>,
    private readonly remove: (draft_id: string) => Promise<void>,
  ) {}

  async ensure(input: CreateConnectionDraft): Promise<NativeConnectionDraft> {
    const normalized = { ...input, base_url: normalizedEndpoint(input.base_url) };
    const key = JSON.stringify([
      normalized.connection_id,
      normalized.provider_id,
      normalized.base_url,
    ]);
    if (this.current && this.current.key !== key) await this.discard();
    // Another operation may have opened a different setup while disposal was
    // pending. Never hand that operation's auth to the superseded caller.
    if (this.current && this.current.key !== key)
      throw new Error('This connection setup is no longer active.');
    if (!this.current) {
      const lease: DraftLease = {
        key,
        promise: this.create(normalized),
        discarded: false,
        consumed: false,
      };
      this.current = lease;
    }
    const lease = this.current;
    try {
      const result = await lease.promise;
      if (lease.discarded || this.current !== lease)
        throw new Error('This connection setup is no longer active.');
      lease.value ??= result;
      return lease.value;
    } catch (error) {
      if (this.current === lease) this.current = undefined;
      throw error;
    }
  }

  update(draft: NativeConnectionDraft): void {
    if (this.current?.value?.draft_id === draft.draft_id) this.current.value = draft;
  }

  consume(draft_id: string): void {
    if (this.current?.value?.draft_id !== draft_id) return;
    this.current.consumed = true;
    this.current = undefined;
  }

  async discard(): Promise<void> {
    const lease = this.current;
    if (!lease) return;
    this.current = undefined;
    lease.discarded = true;
    // Creation can finish after Back or a provider change. Its resulting native
    // draft still needs disposal; a failed create has no resource to remove.
    const draft = await lease.promise.catch(() => undefined);
    if (draft && !lease.consumed) await this.remove(draft.draft_id);
  }
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
