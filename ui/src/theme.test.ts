import { describe, it, expect, afterEach } from 'vitest';
import { themeManager, HostThemePayload } from './theme';

const PALETTE = {
  bg: '#101418',
  fg: '#e6edf3',
  accent: '#7ee787',
  dim: '#8b949e',
  panel: '#161b22',
  warn: '#d29922',
  success: '#3fb950',
  info: '#58a6ff',
  error: '#f85149',
};

const ALL_VARS = [
  '--wf-bg',
  '--wf-fg',
  '--wf-accent',
  '--wf-dim',
  '--wf-panel',
  '--wf-info',
  '--wf-success',
  '--wf-warn',
  '--wf-error',
];

function payload(overrides: Partial<HostThemePayload> = {}): HostThemePayload {
  return { palette: PALETTE, name: 'gruvbox', dark: true, ...overrides };
}

describe('themeManager host palette (koma 0.3.0 theme channel)', () => {
  afterEach(() => {
    themeManager.clearHostPalette();
  });

  it('applyHostPalette sets every --wf-* role custom property from the payload', () => {
    themeManager.applyHostPalette(payload());
    const style = document.documentElement.style;
    expect(style.getPropertyValue('--wf-bg')).toBe('#101418');
    expect(style.getPropertyValue('--wf-fg')).toBe('#e6edf3');
    expect(style.getPropertyValue('--wf-accent')).toBe('#7ee787');
    expect(style.getPropertyValue('--wf-dim')).toBe('#8b949e');
    expect(style.getPropertyValue('--wf-panel')).toBe('#161b22');
    expect(style.getPropertyValue('--wf-info')).toBe('#58a6ff');
    expect(style.getPropertyValue('--wf-success')).toBe('#3fb950');
    expect(style.getPropertyValue('--wf-warn')).toBe('#d29922');
    expect(style.getPropertyValue('--wf-error')).toBe('#f85149');
  });

  it('derives color-scheme + data-theme from the dark flag', () => {
    themeManager.applyHostPalette(payload({ dark: false }));
    expect(document.documentElement.style.getPropertyValue('color-scheme')).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');

    themeManager.applyHostPalette(payload({ dark: true }));
    expect(document.documentElement.style.getPropertyValue('color-scheme')).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('tracks the host theme name and reports isHostThemed', () => {
    expect(themeManager.isHostThemed()).toBe(false);
    themeManager.applyHostPalette(payload({ name: 'nord' }));
    expect(themeManager.isHostThemed()).toBe(true);
    expect(themeManager.getHostThemeName()).toBe('nord');
  });

  it('clearHostPalette removes every override and resets the host state', () => {
    themeManager.applyHostPalette(payload());
    themeManager.clearHostPalette();
    const style = document.documentElement.style;
    for (const cssVar of ALL_VARS) {
      expect(style.getPropertyValue(cssVar)).toBe('');
    }
    expect(style.getPropertyValue('color-scheme')).toBe('');
    expect(themeManager.isHostThemed()).toBe(false);
    expect(themeManager.getHostThemeName()).toBe(null);
  });

  it('notifies host-theme subscribers on apply and clear', () => {
    const seen: (string | null)[] = [];
    const off = themeManager.subscribeHostTheme((name) => seen.push(name));
    themeManager.applyHostPalette(payload({ name: 'solarized' }));
    themeManager.clearHostPalette();
    off();
    expect(seen).toContain('solarized');
    expect(seen).toContain(null);
  });
});
