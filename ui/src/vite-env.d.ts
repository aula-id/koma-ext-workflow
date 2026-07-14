/// <reference types="vite/client" />

interface KomaPanelType {
  send: (payload: any, timeoutMs?: number) => Promise<any>;
  onPush: (handler: (payload: any) => void) => void;
}

declare global {
  interface Window {
    KomaPanel?: KomaPanelType;
  }
}

export {};
