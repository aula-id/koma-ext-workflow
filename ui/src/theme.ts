export type Theme = 'dark' | 'light';

/** The host palette pushed over koma's theme channel (koma 0.3.0). Nine `#rrggbb` role strings;
 * `dark` is the authoritative light/dark flag the panel derives `color-scheme` + `data-theme`
 * from. All fields optional so a partial payload can never throw. */
export interface HostPalette {
  bg?: string;
  fg?: string;
  accent?: string;
  dim?: string;
  panel?: string;
  warn?: string;
  success?: string;
  info?: string;
  error?: string;
  dark?: boolean;
}

/** The full koma `theme` payload: the palette, the theme's display name, and the dark flag. */
export interface HostThemePayload {
  palette: HostPalette;
  name: string;
  dark?: boolean;
}

/** The `--wf-*` custom properties `applyHostPalette` overrides (and `clearHostPalette` removes).
 * The derived surfaces (panel2/border/hover/head/grip, tints) color-mix off --wf-bg/--wf-fg and
 * follow automatically; only these direct roles are pushed — including --wf-panel, which is
 * normally derived but the host provides a real value for, so it is overridden explicitly. */
const HOST_PALETTE_VARS: Record<string, keyof HostPalette> = {
  '--wf-bg': 'bg',
  '--wf-fg': 'fg',
  '--wf-accent': 'accent',
  '--wf-dim': 'dim',
  '--wf-panel': 'panel',
  '--wf-info': 'info',
  '--wf-success': 'success',
  '--wf-warn': 'warn',
  '--wf-error': 'error',
};

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
  /** Non-null while a koma host theme drives the palette; carries the theme's display name. */
  private hostThemeName: string | null = null;
  private hostListeners: Set<(name: string | null) => void> = new Set();

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

  /**
   * Apply a host theme payload (koma 0.3.0 theme channel): override the `--wf-*` role custom
   * properties inline on <html>, set `color-scheme` + `data-theme` from the `dark` flag, and
   * record the theme name so Settings can show the "following koma theme" state. Derived surfaces
   * follow the new bg/fg automatically. Idempotent — safe on every push and the initial query.
   */
  applyHostPalette(payload: HostThemePayload): void {
    const root = document.documentElement;
    const palette = payload.palette || {};
    for (const [cssVar, role] of Object.entries(HOST_PALETTE_VARS)) {
      const value = palette[role];
      if (typeof value === 'string' && value) {
        root.style.setProperty(cssVar, value);
      }
    }
    const dark = payload.dark ?? palette.dark ?? true;
    root.style.setProperty('color-scheme', dark ? 'dark' : 'light');
    root.setAttribute('data-theme', dark ? 'dark' : 'light');
    this.hostThemeName = payload.name || 'koma';
    this.hostListeners.forEach((listener) => listener(this.hostThemeName));
  }

  /**
   * Drop the host palette overrides and revert to the manual (localStorage) theme: remove every
   * `--wf-*` override + the inline `color-scheme` so the tokens.css values take over again.
   */
  clearHostPalette(): void {
    const root = document.documentElement;
    for (const cssVar of Object.keys(HOST_PALETTE_VARS)) {
      root.style.removeProperty(cssVar);
    }
    root.style.removeProperty('color-scheme');
    this.hostThemeName = null;
    this.applyTheme(); // re-assert the manual data-theme
    this.hostListeners.forEach((listener) => listener(null));
  }

  /** The active host theme's display name, or `null` when no host theme is in effect (manual). */
  getHostThemeName(): string | null {
    return this.hostThemeName;
  }

  /** Whether a host (koma) theme is currently driving the palette. */
  isHostThemed(): boolean {
    return this.hostThemeName !== null;
  }

  /**
   * Subscribe to host-theme on/off + name changes. Settings uses this to swap the manual dark/
   * light toggle for the "following koma theme (<name>)" line while a host theme is active.
   */
  subscribeHostTheme(listener: (name: string | null) => void): () => void {
    this.hostListeners.add(listener);
    return () => this.hostListeners.delete(listener);
  }
}

export const themeManager = new ThemeManager();
