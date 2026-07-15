/// <reference types="vite/client" />

interface KomaPanelType {
  send: (payload: any, timeoutMs?: number) => Promise<any>;
  onPush: (handler: (payload: any) => void) => void;
  /** koma 0.3.0 theme channel (koma-panel.js). Optional: a host/mock that predates it,
   * or the standalone mock harness, simply omits these and the panel keeps its manual toggle. */
  getTheme?: (timeoutMs?: number) => Promise<any>;
  onTheme?: (handler: (payload: any) => void) => void;
}

declare global {
  interface Window {
    KomaPanel?: KomaPanelType;
  }
}

export {};
