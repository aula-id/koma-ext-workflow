export type Theme = 'dark' | 'light';

/**
 * Theme switching is attribute-driven: the actual color values for each theme live
 * exactly once, in `tokens.css` (`:root` for dark, `:root[data-theme='light']` for
 * light) — sourced from koma's host palette (see that file's header comment). This
 * class only toggles the `data-theme` attribute and persists the choice; it does not
 * duplicate any color literal.
 *
 * koma has no separate "milk" palette (see tokens.css comment), so only dark/light
 * are offered here.
 */
export class ThemeManager {
  private currentTheme: Theme = 'dark';
  private listeners: Set<(theme: Theme) => void> = new Set();

  constructor() {
    this.loadTheme();
    this.applyTheme();
    this.setupReducedMotion();
  }

  private loadTheme(): void {
    const saved = localStorage.getItem('wf-theme');
    if (saved === 'light' || saved === 'dark') {
      this.currentTheme = saved;
    } else if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
      this.currentTheme = 'light';
    }
  }

  private applyTheme(): void {
    const root = document.documentElement;
    root.setAttribute('data-theme', this.currentTheme);
    localStorage.setItem('wf-theme', this.currentTheme);
  }

  private setupReducedMotion(): void {
    // Guarded like `loadTheme`'s matchMedia read above: some embedding webviews (and
    // jsdom test environments) don't implement `matchMedia` at all — treat that as
    // "no preference" rather than crashing the whole module on import.
    const prefersReducedMotion = Boolean(
      window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches,
    );
    if (prefersReducedMotion) {
      document.documentElement.style.setProperty('--wf-animation-duration', '0s');
    } else {
      document.documentElement.style.setProperty('--wf-animation-duration', '0.2s');
    }
  }

  getTheme(): Theme {
    return this.currentTheme;
  }

  setTheme(theme: Theme): void {
    this.currentTheme = theme;
    this.applyTheme();
    this.listeners.forEach((listener) => listener(theme));
  }

  subscribe(listener: (theme: Theme) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

export const themeManager = new ThemeManager();
