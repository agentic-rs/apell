// Each view gets its own broker and MessagePort. A late reply from an old
// document must never close a reloaded or newly opened view.
export class AppletBack {
  private next = 0;
  private pending?: { request_id: string; timer: ReturnType<typeof setTimeout>; close: () => void };
  private disposed = false;

  constructor(private readonly port: Pick<MessagePort, 'postMessage'>) {}

  request(close: () => void): void {
    if (this.disposed || this.pending) return;
    const request_id = String(++this.next);
    // If the applet doesn't answer, stay on its current view. Its handler
    // might already have changed routes or be waiting for confirmation.
    const timer = setTimeout(() => this.clear(), 1_000);
    this.pending = { request_id, timer, close };
    try {
      this.port.postMessage({ type: 'navigation_back', request_id });
    } catch {
      this.clear();
    }
  }

  receive(value: unknown): void {
    const pending = this.pending;
    if (!pending || !value || typeof value !== 'object') return;
    const { type, request_id, handled } = value as Record<string, unknown>;
    if (
      type !== 'navigation_back_result' ||
      request_id !== pending.request_id ||
      typeof handled !== 'boolean'
    )
      return;
    this.clear();
    if (!handled) pending.close();
  }

  dispose(): void {
    this.disposed = true;
    this.clear();
  }

  private clear(): void {
    if (this.pending) clearTimeout(this.pending.timer);
    this.pending = undefined;
  }
}
