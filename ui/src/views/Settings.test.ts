import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import { useStore } from '../store';
import Settings, { clampMaxWorkers } from './Settings';
import { bridge } from '../bridge';

// Regression: this file used to test a copy-pasted reimplementation of
// `clampMaxWorkers` and a hand-built `config_set` payload object — neither of which
// ever imported or rendered the real `Settings.tsx`, so a bug in the actual component
// (e.g. the `keepDesks` field silently not making it into the submitted payload) could
// never fail this suite. It now imports the real exported helper and mounts the real
// component (react-dom, matching Dashboard.test.tsx's pattern — no @testing-library/
// react dependency in this project).
vi.mock('../bridge', () => ({
  bridge: { send: vi.fn() },
}));

describe('clampMaxWorkers (the real exported function, not a copy)', () => {
  it('clamps 0 to 1', () => {
    expect(clampMaxWorkers(0)).toBe(1);
  });

  it('keeps 1..4 unchanged', () => {
    expect(clampMaxWorkers(1)).toBe(1);
    expect(clampMaxWorkers(2)).toBe(2);
    expect(clampMaxWorkers(3)).toBe(3);
    expect(clampMaxWorkers(4)).toBe(4);
  });

  it('clamps above 4 down to 4', () => {
    expect(clampMaxWorkers(5)).toBe(4);
    expect(clampMaxWorkers(100)).toBe(4);
  });

  it('clamps negative values up to 1', () => {
    expect(clampMaxWorkers(-5)).toBe(1);
  });
});

describe('Settings (real component, rendered)', () => {
  let container: HTMLDivElement;
  let root: Root;

  function seedProject(configOverrides: Record<string, unknown> = {}) {
    act(() => {
      useStore.getState().updateSnapshot({
        kind: 'snapshot',
        seq: 1,
        projects: [
          {
            id: 'p1',
            name: 'Auth Service',
            phase: { kind: 'running' },
            tasks: [],
            config: {
              maxWorkers: 2,
              bounceBudget: 3,
              workerModel: 'claude-sonnet',
              reviewerModel: 'claude-opus',
              keepDesks: false,
              ...configOverrides,
            },
          },
        ],
      });
    });
  }

  function renderSettings() {
    act(() => {
      root.render(React.createElement(Settings, { projectId: 'p1' }));
    });
  }

  function setInputValue(input: HTMLInputElement, value: string) {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')!.set!;
    act(() => {
      setter.call(input, value);
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
  }

  async function submitForm() {
    const submit = container.querySelector('[data-testid="settings-submit"]') as HTMLButtonElement;
    await act(async () => {
      submit.click();
      await Promise.resolve();
    });
  }

  beforeEach(() => {
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    useStore.setState({ snapshot: null, projects: [] });
    vi.mocked(bridge.send).mockReset();
    vi.mocked(bridge.send).mockResolvedValue({ ok: true, accepted: true });
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => {
      root.unmount();
    });
    container.remove();
  });

  it('renders the keepDesks toggle reflecting the project config, not a hardcoded default', () => {
    seedProject({ keepDesks: true });
    renderSettings();

    const toggle = container.querySelector('[data-testid="settings-keep-desks-toggle"]');
    expect(toggle).toBeTruthy();
    expect(toggle?.getAttribute('aria-checked')).toBe('true');
  });

  it('submits config_set with keepDesks flipped after clicking the toggle (the fix under test)', async () => {
    seedProject({ keepDesks: false });
    renderSettings();

    const toggle = container.querySelector('[data-testid="settings-keep-desks-toggle"]') as HTMLButtonElement;
    expect(toggle.getAttribute('aria-checked')).toBe('false');
    act(() => {
      toggle.click();
    });
    expect(toggle.getAttribute('aria-checked')).toBe('true');

    await submitForm();

    expect(bridge.send).toHaveBeenCalledWith(
      expect.objectContaining({ op: 'config_set', project: 'p1', keepDesks: true }),
    );
  });

  it('submits the clamped maxWorkers value from the real input, via the real clamp function', async () => {
    seedProject();
    renderSettings();

    const input = container.querySelector('[data-testid="settings-max-workers"]') as HTMLInputElement;
    setInputValue(input, '99');

    await submitForm();

    expect(bridge.send).toHaveBeenCalledWith(expect.objectContaining({ maxWorkers: clampMaxWorkers(99) }));
    expect(bridge.send).toHaveBeenCalledWith(expect.objectContaining({ maxWorkers: 4 }));
  });

  it('never submits per-project model overrides — models are bound in the koma sub-agent sidebar', async () => {
    // Product decision (2026-07-15): worker/reviewer model fields were removed
    // from Settings entirely; the contributed sub-agents' models are bound in
    // koma's sidebar. config_set must not carry model keys AT ALL (not even
    // undefined), and no free-text model inputs may render.
    seedProject({ workerModel: 'claude-sonnet', reviewerModel: 'claude-opus' });
    renderSettings();

    expect(container.querySelectorAll('input[type="text"]').length).toBe(0);

    await submitForm();

    const payload = vi.mocked(bridge.send).mock.calls.at(-1)![0] as Record<string, unknown>;
    expect(payload.op).toBe('config_set');
    expect('workerModel' in payload).toBe(false);
    expect('reviewerModel' in payload).toBe(false);
  });

  it('shows a success message on a clean save and an access-denied message on a grant-denied error', async () => {
    seedProject();
    renderSettings();

    await submitForm();
    expect(container.textContent).toContain('Settings saved');

    vi.mocked(bridge.send).mockResolvedValueOnce({ error: 'grant denied: config_set' });
    await submitForm();
    expect(container.textContent).toContain('Access denied');
  });
});
