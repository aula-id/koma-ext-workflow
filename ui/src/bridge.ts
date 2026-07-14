export interface Snapshot {
  kind: 'snapshot';
  seq: number;
  projects: any[];
  truncated?: boolean;
}

export interface BridgeListener {
  (snapshot: Snapshot): void;
}

export class Bridge {
  private listeners: BridgeListener[] = [];
  private lastSeq: number = -1;

  constructor() {
    this.setupListeners();
    this.setupVisibilityHandler();
  }

  private setupListeners(): void {
    if (!window.KomaPanel) {
      console.error('KomaPanel not available');
      return;
    }

    window.KomaPanel.onPush((payload: Snapshot) => {
      if (payload.kind !== 'snapshot') {
        return;
      }

      if (payload.seq < this.lastSeq) {
        return;
      }

      this.lastSeq = payload.seq;
      this.notifyListeners(payload);
    });
  }

  private setupVisibilityHandler(): void {
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) {
        this.rehydrate();
      }
    });
  }

  async send(payload: any, timeoutMs?: number): Promise<any> {
    if (!window.KomaPanel) {
      throw new Error('KomaPanel not available');
    }
    return window.KomaPanel.send(payload, timeoutMs);
  }

  async hello(uiVersion: string): Promise<Snapshot> {
    const result = await this.send({ op: 'hello', uiVersion });
    if (result.snapshot && result.snapshot.kind === 'snapshot') {
      this.lastSeq = result.snapshot.seq;
      this.notifyListeners(result.snapshot);
      return result.snapshot;
    }
    throw new Error('Invalid hello response');
  }

  async state(project?: string): Promise<Snapshot> {
    const result = await this.send({ op: 'state', ...(project && { project }) });
    if (result.snapshot && result.snapshot.kind === 'snapshot') {
      if (result.snapshot.seq > this.lastSeq) {
        this.lastSeq = result.snapshot.seq;
        this.notifyListeners(result.snapshot);
      }
      return result.snapshot;
    }
    throw new Error('Invalid state response');
  }

  async rehydrate(): Promise<void> {
    try {
      await this.state();
    } catch (error) {
      console.error('Failed to rehydrate:', error);
    }
  }

  onSnapshot(listener: BridgeListener): () => void {
    this.listeners.push(listener);
    return () => {
      const idx = this.listeners.indexOf(listener);
      if (idx >= 0) {
        this.listeners.splice(idx, 1);
      }
    };
  }

  private notifyListeners(snapshot: Snapshot): void {
    this.listeners.forEach((listener) => {
      try {
        listener(snapshot);
      } catch (error) {
        console.error('Listener error:', error);
      }
    });
  }
}

export const bridge = new Bridge();
