export type Theme = 'dark' | 'light' | 'milk';

interface ThemeColors {
  bg: string;
  bgSecondary: string;
  fg: string;
  fgSecondary: string;
  accentGreen: string;
  accentOrange: string;
  accentPink: string;
  accentBlue: string;
  accentPurple: string;
  accentYellow: string;
}

const THEMES: Record<Theme, ThemeColors> = {
  dark: {
    bg: '#1e1f1c',
    bgSecondary: '#26271f',
    fg: '#f8f8f2',
    fgSecondary: '#e0e0d9',
    accentGreen: '#a6e22e',
    accentOrange: '#fd971f',
    accentPink: '#f92672',
    accentBlue: '#66d9ef',
    accentPurple: '#ae81ff',
    accentYellow: '#e6db74',
  },
  light: {
    bg: '#fafafa',
    bgSecondary: '#f0f0f0',
    fg: '#1a1a1a',
    fgSecondary: '#4a4a4a',
    accentGreen: '#7ab000',
    accentOrange: '#d47f1f',
    accentPink: '#d60f7e',
    accentBlue: '#0073b6',
    accentPurple: '#8b5fbf',
    accentYellow: '#b89c00',
  },
  milk: {
    bg: '#ffffff',
    bgSecondary: '#f5f5f5',
    fg: '#000000',
    fgSecondary: '#505050',
    accentGreen: '#7ab000',
    accentOrange: '#d47f1f',
    accentPink: '#d60f7e',
    accentBlue: '#0073b6',
    accentPurple: '#8b5fbf',
    accentYellow: '#b89c00',
  },
};

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
    if (saved === 'light' || saved === 'milk' || saved === 'dark') {
      this.currentTheme = saved;
    } else if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
      this.currentTheme = 'light';
    }
  }

  private applyTheme(): void {
    const colors = THEMES[this.currentTheme];
    const root = document.documentElement;
    root.style.setProperty('--wf-bg', colors.bg);
    root.style.setProperty('--wf-bg-secondary', colors.bgSecondary);
    root.style.setProperty('--wf-fg', colors.fg);
    root.style.setProperty('--wf-fg-secondary', colors.fgSecondary);
    root.style.setProperty('--wf-accent-green', colors.accentGreen);
    root.style.setProperty('--wf-accent-orange', colors.accentOrange);
    root.style.setProperty('--wf-accent-pink', colors.accentPink);
    root.style.setProperty('--wf-accent-blue', colors.accentBlue);
    root.style.setProperty('--wf-accent-purple', colors.accentPurple);
    root.style.setProperty('--wf-accent-yellow', colors.accentYellow);
    localStorage.setItem('wf-theme', this.currentTheme);
  }

  private setupReducedMotion(): void {
    const prefersReducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
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
