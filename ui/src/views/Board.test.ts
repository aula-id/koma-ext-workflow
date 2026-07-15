import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import { useStore } from '../store';
import Board, { guardCardMove } from './Board';
import { bridge } from '../bridge';

// Same pattern as Settings.test.ts: mock the bridge door so `bridge.send` calls are
// inspectable, and `onSnapshot`/`state` are harmless no-ops (the store is seeded
// directly via `updateSnapshot`, not through a pushed snapshot).
vi.mock('../bridge', () => ({
  bridge: {
    send: vi.fn(),
    onSnapshot: vi.fn(() => () => {}),
    state: vi.fn().mockResolvedValue({ kind: 'snapshot', seq: 0, projects: [] }),
  },
}));

describe('guardCardMove', () => {
  it('allows backlog -> todo (groom)', () => {
    expect(guardCardMove('backlog', 'todo', false)).toEqual({ legal: true });
  });

  it('rejects backlog -> onprogress (no direct edge)', () => {
    const result = guardCardMove('backlog', 'onprogress', false);
    expect(result.legal).toBe(false);
    expect(result.reason).toBeTruthy();
  });

  it('rejects todo -> anything (dispatch is kernel-driven, not a manual drag)', () => {
    expect(guardCardMove('todo', 'onprogress', false).legal).toBe(false);
    expect(guardCardMove('todo', 'backlog', false).legal).toBe(false);
  });

  it('allows onprogress -> review (manual send-to-review)', () => {
    expect(guardCardMove('onprogress', 'review', false)).toEqual({ legal: true });
  });

  it('requires killWorker when dragging onprogress -> todo', () => {
    const withoutKill = guardCardMove('onprogress', 'todo', false);
    expect(withoutKill.legal).toBe(false);
    expect(withoutKill.requiresKillWorker).toBe(true);

    const withKill = guardCardMove('onprogress', 'todo', true);
    expect(withKill).toEqual({ legal: true });
  });

  it('allows review -> done and review -> todo', () => {
    expect(guardCardMove('review', 'done', false)).toEqual({ legal: true });
    expect(guardCardMove('review', 'todo', false)).toEqual({ legal: true });
  });

  it('allows parked -> todo (unpark)', () => {
    expect(guardCardMove('parked', 'todo', false)).toEqual({ legal: true });
  });

  it('rejects any move out of done (terminal state)', () => {
    expect(guardCardMove('done', 'todo', false).legal).toBe(false);
    expect(guardCardMove('done', 'backlog', false).legal).toBe(false);
  });

  it('rejects a no-op move to the same column when not a legal edge', () => {
    expect(guardCardMove('done', 'done', false).legal).toBe(false);
  });
});

/**
 * "Skip research" button (design-speedup item 7, workflow_skip): the same panel-op door
 * pattern as interrupt/resume above (ConfirmButton two-step, `bridge.send`), gated on the
 * structured `researchActive` snapshot field the office map already reads via
 * `isResearchLive` — not a string-match on trace lines.
 */
describe('Board — skip research button', () => {
  let container: HTMLDivElement;
  let root: Root;

  function seedProject(overrides: Record<string, unknown> = {}) {
    act(() => {
      useStore.getState().updateSnapshot({
        kind: 'snapshot',
        seq: 1,
        projects: [
          {
            id: 'p1',
            name: 'Auth Service',
            phase: { kind: 'drafting' },
            tasks: [],
            ...overrides,
          },
        ],
      });
    });
  }

  function renderBoard() {
    act(() => {
      root.render(React.createElement(Board, { projectId: 'p1' }));
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

  it('does not render while no research is in flight', () => {
    seedProject({ researchActive: false });
    renderBoard();
    expect(container.querySelector('[data-testid="skip-research-btn"]')).toBeFalsy();
  });

  it('renders only while research is in flight (researchActive)', () => {
    seedProject({ researchActive: true });
    renderBoard();
    expect(container.querySelector('[data-testid="skip-research-btn"]')).toBeTruthy();
  });

  it('also renders off the raw research binding (research present, no researchActive flag)', () => {
    seedProject({ research: { extAgentId: 1 } });
    renderBoard();
    expect(container.querySelector('[data-testid="skip-research-btn"]')).toBeTruthy();
  });

  it('sends the skip op for the active project on confirm (two-step, like interrupt)', async () => {
    seedProject({ researchActive: true });
    renderBoard();

    const btn = container.querySelector('[data-testid="skip-research-btn"]') as HTMLButtonElement;
    expect(btn).toBeTruthy();

    // First click arms the button; nothing is sent yet (ConfirmButton two-step, no
    // window.confirm — wry webviews have none).
    act(() => {
      btn.click();
    });
    expect(bridge.send).not.toHaveBeenCalledWith(expect.objectContaining({ op: 'skip' }));

    // Second click within the arm window fires the confirmed action.
    await act(async () => {
      btn.click();
      await Promise.resolve();
    });
    expect(bridge.send).toHaveBeenCalledWith({ op: 'skip', project: 'p1' });
  });

  it('disappears once research settles (a follow-up snapshot with researchActive: false)', () => {
    seedProject({ researchActive: true });
    renderBoard();
    expect(container.querySelector('[data-testid="skip-research-btn"]')).toBeTruthy();

    seedProject({ researchActive: false });
    expect(container.querySelector('[data-testid="skip-research-btn"]')).toBeFalsy();
  });
});
