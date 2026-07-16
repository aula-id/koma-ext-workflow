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

/**
 * SDLC track badge (feature: sdlc-triage, review finding MINOR): `track` rides the wire
 * (office-core digest.rs `"track"`) but was not rendered anywhere in the panel — the header
 * now shows a small flat badge next to the phase indicator when a track is present.
 */
describe('Board — SDLC track badge', () => {
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

  it('renders the badge with the track label when track is "patch"', () => {
    seedProject({ track: 'patch' });
    renderBoard();
    const badge = container.querySelector('[data-testid="project-track-badge"]');
    expect(badge).toBeTruthy();
    expect(badge?.textContent).toBe('patch');
  });

  it('renders nothing when track is absent (back-compat, older snapshot)', () => {
    seedProject({});
    renderBoard();
    expect(container.querySelector('[data-testid="project-track-badge"]')).toBeFalsy();
  });
});

/**
 * Design-stage placeholder cards (feature: design-stage-cards): while a project carries
 * `designStages` on its snapshot (pre-Ready — Drafting or paused mid-Drafting), the board
 * renders them in the matching column instead of leaving it empty. Once `designStages` is
 * absent (Ready+, or an older snapshot), the classic board (real task cards, or the
 * client-derived `docCards` fallback) renders unchanged.
 */
describe('Board — design-stage cards', () => {
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
      root.render(React.createElement(Board, { projectId: 'p1', initialTab: 'board' }));
    });
  }

  beforeEach(() => {
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    useStore.setState({ snapshot: null, projects: [] });
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

  it('renders one card per stage, in the column matching its status, with the note as subtitle', () => {
    seedProject({
      designStages: [
        { id: 'triage', label: 'Triage', status: 'done', note: 'project' },
        { id: 'prd', label: 'PRD', status: 'done', note: 'verified — clean' },
        { id: 'research', label: 'Research', status: 'done', note: 'skipped — stack well-known' },
        { id: 'trdcrd', label: 'TRD+CRD', status: 'inProgress' },
        { id: 'breakdown', label: 'Breakdown', status: 'todo' },
      ],
    });
    renderBoard();

    const cards = container.querySelectorAll('[data-testid="design-stage-card"]');
    expect(cards.length).toBe(5);

    const research = container.querySelector('[data-stage-id="research"]');
    expect(research?.textContent).toContain('Research');
    expect(research?.textContent).toContain('skipped — stack well-known');

    const trdcrd = container.querySelector('[data-stage-id="trdcrd"]');
    expect(trdcrd?.textContent).toContain('TRD+CRD');

    const breakdown = container.querySelector('[data-stage-id="breakdown"]');
    expect(breakdown?.textContent).toContain('Breakdown');
  });

  it('does not render the client-derived docCards while designStages is present', () => {
    seedProject({
      prdMarkdown: '# PRD',
      designStages: [{ id: 'triage', label: 'Triage', status: 'inProgress' }],
    });
    renderBoard();
    expect(container.querySelector('[data-testid="doc-card"]')).toBeFalsy();
    expect(container.querySelector('[data-testid="design-stage-card"]')).toBeTruthy();
  });

  it('renders the classic board (docCards, no design-stage cards) once designStages is absent', () => {
    seedProject({ prdMarkdown: '# PRD' });
    renderBoard();
    expect(container.querySelector('[data-testid="design-stage-card"]')).toBeFalsy();
    expect(container.querySelector('[data-testid="doc-card"]')).toBeTruthy();
  });

  it('renders real task cards unchanged once designStages is absent (Ready+)', () => {
    seedProject({
      phase: { kind: 'ready' },
      tasks: [
        {
          id: 't1',
          title: 'Do the thing',
          column: 'todo',
          state: 'todo',
          priority: 0,
          blockedBy: [],
          bounces: 0,
        },
      ],
    });
    renderBoard();
    expect(container.querySelector('[data-testid="design-stage-card"]')).toBeFalsy();
    expect(container.querySelector('[data-task-id="t1"]')).toBeTruthy();
  });
});

/**
 * Sprint plan strip (feature: sprint-list): `sprints[]` fed OfficeMap's badge + meeting
 * room already (feature: sprints), but the Board itself never rendered the plan — this
 * strip is the first place a user can see all sprints at once, between the tab header and
 * the kanban columns.
 */
describe('Board — sprint plan strip', () => {
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
            phase: { kind: 'ready' },
            tasks: [],
            ...overrides,
          },
        ],
      });
    });
  }

  function renderBoard() {
    act(() => {
      root.render(React.createElement(Board, { projectId: 'p1', initialTab: 'board' }));
    });
  }

  beforeEach(() => {
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    useStore.setState({ snapshot: null, projects: [] });
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

  const threeSprints = [
    { index: 0, goal: 'Stand up auth', status: 'done', total: 3, done: 3, tasks: ['p1/e1/s1/t1'] },
    { index: 1, goal: 'Wire up billing', status: 'active', total: 4, done: 2, tasks: ['p1/e1/s1/t2'] },
    { index: 2, goal: 'Polish onboarding', status: 'inreview', total: 2, done: 2, tasks: ['p1/e1/s1/t3'] },
  ];

  it('renders nothing when sprints is absent (back-compat)', () => {
    seedProject({});
    renderBoard();
    expect(container.querySelector('[data-testid="sprint-strip"]')).toBeFalsy();
  });

  it('renders nothing when sprints is an empty array (back-compat)', () => {
    seedProject({ sprints: [] });
    renderBoard();
    expect(container.querySelector('[data-testid="sprint-strip"]')).toBeFalsy();
  });

  it('renders one row per sprint, in order, with status label + progress', () => {
    seedProject({ sprints: threeSprints });
    renderBoard();

    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    expect(rows.length).toBe(3);
    expect(rows[0].getAttribute('data-sprint-index')).toBe('0');
    expect(rows[1].getAttribute('data-sprint-index')).toBe('1');
    expect(rows[2].getAttribute('data-sprint-index')).toBe('2');

    expect(rows[0].textContent).toContain('S1');
    expect(rows[0].textContent).toContain('Stand up auth');
    expect(rows[0].textContent).toContain('done');
    expect(rows[0].textContent).toContain('3/3');

    expect(rows[1].textContent).toContain('S2');
    expect(rows[1].textContent).toContain('active');
    expect(rows[1].textContent).toContain('2/4');
  });

  it('maps the wire "inreview" status to the "in review" label', () => {
    seedProject({ sprints: threeSprints });
    renderBoard();
    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    expect(rows[2].textContent).toContain('in review');
    expect(rows[2].textContent).not.toContain('inreview');
  });

  it('visually highlights the active sprint (accent border/background)', () => {
    seedProject({ sprints: threeSprints });
    renderBoard();
    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    const activeRow = rows[1] as HTMLElement;
    expect(activeRow.style.borderLeft).toContain('var(--wf-accent)');
  });

  it('gives the inreview sprint a distinct (pulse) treatment', () => {
    seedProject({ sprints: threeSprints });
    renderBoard();
    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    const reviewChip = rows[2].querySelector('[data-testid="sprint-status-chip"]');
    // The pulsing dot is an extra child node inside the status chip (see DocCardView /
    // DesignStageCardView's identical opacity-pulse recipe); pending/active/done chips
    // carry only the label text node.
    expect(reviewChip?.children.length).toBeGreaterThan(0);
  });

  it('clicking a sprint row toggles a one-line task expansion, resolving ids to titles', () => {
    seedProject({
      sprints: threeSprints,
      tasks: [{ id: 'p1/e1/s1/t2', title: 'Wire Stripe webhook', column: 'todo', state: 'todo', priority: 0, blockedBy: [], bounces: 0 }],
    });
    renderBoard();

    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    expect(container.querySelector('[data-testid="sprint-row-expanded"]')).toBeFalsy();

    act(() => {
      (rows[1] as HTMLElement).click();
    });
    const expanded = container.querySelector('[data-testid="sprint-row-expanded"]');
    expect(expanded).toBeTruthy();
    expect(expanded?.textContent).toContain('Wire Stripe webhook');

    // Clicking again collapses it.
    act(() => {
      (rows[1] as HTMLElement).click();
    });
    expect(container.querySelector('[data-testid="sprint-row-expanded"]')).toBeFalsy();
  });

  it('falls back to the bare task slug when a sprint task id has no matching project task', () => {
    seedProject({ sprints: threeSprints, tasks: [] });
    renderBoard();
    const rows = container.querySelectorAll('[data-testid="sprint-row"]');
    act(() => {
      (rows[0] as HTMLElement).click();
    });
    const expanded = container.querySelector('[data-testid="sprint-row-expanded"]');
    expect(expanded?.textContent).toContain('t1');
  });
});
