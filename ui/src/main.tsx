import React from 'react';
import ReactDOM from 'react-dom/client';
import './index.css';
import { themeManager, Theme } from './theme';

const params = new URLSearchParams(window.location.search);

// Deterministic initial theme for screenshots/deep-links (?theme=dark|light|milk),
// overriding whatever was previously saved to localStorage.
const themeParam = params.get('theme');
if (themeParam === 'dark' || themeParam === 'light' || themeParam === 'milk') {
  themeManager.setTheme(themeParam as Theme);
}

/**
 * `App` (and everything it imports, transitively including `./bridge`, whose
 * `Bridge` singleton is constructed at module-import time and immediately reads
 * `window.KomaPanel`) is loaded via a dynamic import so the mock harness — when
 * `?mock=1` is present — can install its `window.KomaPanel` stub FIRST. Real
 * host/host-less runs (no `?mock=1`) never import `./mock` at all: it is fully
 * tree-shaken out of that path.
 */
async function bootstrap(): Promise<void> {
  if (params.get('mock') === '1') {
    const { installMockBridge } = await import('./mock');
    installMockBridge();
  }

  const { default: App } = await import('./App');

  const root = document.getElementById('root');
  if (!root) {
    throw new Error('Root element not found');
  }

  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}

void bootstrap();
