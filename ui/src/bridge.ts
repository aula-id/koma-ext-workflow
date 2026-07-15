import type { HostThemePayload } from './theme';

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

  /**
   * Subscribe to koma host theme changes (koma 0.3.0). Delegates to `window.KomaPanel.onTheme`,
   * the dedicated theme channel in koma-panel.js — theme pushes ride a distinct `kind:"theme"`
   * envelope, NOT the `onPush` snapshot channel, so they never collide with board snapshots. A
   * host/mock without the theme channel (standalone or the mock harness) is a silent no-op, so
   * the panel keeps its manual dark/light toggle. Fires immediately with the current theme if one
   * has already arrived. Returns a no-op unsubscribe (koma-panel.js has no off).
   */
  onTheme(listener: (payload: HostThemePayload) => void): () => void {
    const kp = window.KomaPanel;
    if (kp && typeof kp.onTheme === 'function') {
      kp.onTheme((payload: any) => {
        try {
          listener(payload as HostThemePayload);
        } catch (error) {
          console.error('Theme listener error:', error);
        }
      });
    }
    return () => {};
  }

  /**
   * Query the host for the current theme (koma 0.3.0), resolving `null` when no host theme is
   * available (standalone / mock / a host build without the theme channel) so callers keep the
   * manual toggle. Delegates to `window.KomaPanel.getTheme`, which sends the `kind:"theme?"`
   * query and resolves the reply's `payload`.
   */
  async getTheme(): Promise<HostThemePayload | null> {
    const kp = window.KomaPanel;
    if (!kp || typeof kp.getTheme !== 'function') {
      return null;
    }
    try {
      const payload = await kp.getTheme();
      if (payload && payload.palette) {
        return payload as HostThemePayload;
      }
      return null;
    } catch {
      return null;
    }
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
