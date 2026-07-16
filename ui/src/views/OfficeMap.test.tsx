import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import OfficeMap, { __resetOfficeTickForTests } from './OfficeMap';

/**
 * Component smoke test: the office view renders a desk per occupied persona and opens the task
 * drawer when a desk is clicked. Pure layout/presence logic is covered exhaustively in
 * lib/officeLayout.test.ts; this pins the render + click wiring.
 */
describe('OfficeMap', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    // The office's animation tick now persists across mounts by design (feature: ambient-idle-life
    // bug 2b — a real remount, e.g. a tab switch, should resume the clock, not restart it). Reset
    // it here so each test still starts from a known tick=0, the same guarantee a fresh page load
    // gives production; otherwise tick state would leak across `it()` blocks in this file and any
    // "at mount" assertion (e.g. the meeting-room ceremony's first transcript line) would depend on
    // run order.
    __resetOfficeTickForTests();
    // jsdom ships no matchMedia; stub reduced-motion = false so the component's guard is exercised.
    if (typeof window.matchMedia !== 'function') {
      (window as any).matchMedia = () => ({
        matches: false,
        addEventListener() {},
        removeEventListener() {},
        addListener() {},
        removeListener() {},
      });
    }
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  const project: any = {
    id: 'p',
    name: 'P',
    phase: { kind: 'running' },
    config: { maxWorkers: 4 },
    tasks: [
      { id: 'p/a', title: 'Working task', state: 'onprogress', priority: 7, persona: 'nova' },
      { id: 'p/b', title: 'Reviewing task', state: 'review', priority: 6, persona: 'tetsuo' },
      { id: 'p/c', title: 'Parked task', state: 'parked', priority: 9, persona: 'bob' },
    ],
  };

  it('renders a desk per occupied persona and opens the drawer on desk click', () => {
    let clicked: string | null = null;
    act(() => {
      root.render(
        React.createElement(OfficeMap, { project, onTaskClick: (id: string) => { clicked = id; } }),
      );
    });

    const desks = container.querySelectorAll('[data-testid="office-desk"]');
    expect(desks.length).toBeGreaterThanOrEqual(3);

    const personas = Array.from(container.querySelectorAll('[data-persona]'))
      .map((d) => d.getAttribute('data-persona'));
    expect(personas).toContain('nova');
    expect(personas).toContain('tetsuo');
    expect(personas).toContain('bob');

    // Clicking the working persona's desk opens its task in the drawer.
    const novaDesk = container.querySelector('[data-persona="nova"]') as HTMLElement;
    act(() => {
      novaDesk.click();
    });
    expect(clicked).toBe('p/a');
  });

  it('shows the PM waiting on the user with a "?" bubble when assumptions are pending', () => {
    const waiting: any = {
      id: 'w',
      name: 'Waiting',
      phase: { kind: 'drafting' },
      config: { maxWorkers: 2 },
      tasks: [],
      officeActivity: { label: 'waiting on you — 2 assumptions', sinceMs: 0 },
    };
    act(() => {
      root.render(React.createElement(OfficeMap, { project: waiting, onTaskClick: () => {} }));
    });

    expect(container.textContent).toContain('front office - waiting on you');
    expect(container.querySelector('[data-testid="pm-waiting-bubble"]')).not.toBeNull();
  });

  it('renders the classic desk-grid office unchanged when sprints/activeSprint are absent (back-compat)', () => {
    act(() => {
      root.render(React.createElement(OfficeMap, { project, onTaskClick: () => {} }));
    });
    expect(container.querySelector('[data-testid="sprint-badge"]')).toBeNull();
    expect(container.querySelectorAll('[data-testid="meeting-seat"][data-role="pm"]').length).toBe(0);
    expect(container.querySelectorAll('[data-testid="meeting-seat"][data-role="worker"]').length).toBe(0);
    expect(container.querySelectorAll('[data-testid="office-desk"]').length).toBeGreaterThanOrEqual(3);
  });

  // -------------------------------------------------------------------------
  // Sprint-review meeting room (feature: sprints)
  // -------------------------------------------------------------------------

  /** A project whose lone sprint is InReview: nova + mika worked its two (now Done) tasks, and
   * the ceremony transcript has one line per fixed role in the order it plays back. */
  const reviewProject: any = {
    id: 'r',
    name: 'R',
    phase: { kind: 'running' },
    config: { maxWorkers: 2 },
    tasks: [
      { id: 'r/a', title: 'Task A', state: 'done', priority: 5, persona: 'nova' },
      { id: 'r/b', title: 'Task B', state: 'done', priority: 4, persona: 'mika' },
    ],
    sprints: [
      {
        index: 0,
        goal: 'Foundation',
        status: 'inreview',
        total: 2,
        done: 2,
        tasks: ['r/a', 'r/b'],
        transcript: [
          { speaker: 'nova', text: 'built the client' },
          { speaker: 'reviewer', text: '2 tasks passed' },
          { speaker: 'researcher', text: '(observing)' },
        ],
      },
    ],
    activeSprint: { index: 0, count: 1, goal: 'Foundation', total: 2, done: 2, inReview: true },
  };

  describe('meeting-room scene', () => {
    it('swaps the desk grid for the meeting table while inReview, seats PM/reviewer/researcher/workers, and replays the transcript one line at a time', () => {
      vi.useFakeTimers();
      try {
        act(() => {
          root.render(React.createElement(OfficeMap, { project: reviewProject, onTaskClick: () => {} }));
        });

        // No desk cells while in review — the scene swapped to the meeting table.
        expect(container.querySelectorAll('[data-testid="office-desk"]').length).toBe(0);

        const seats = Array.from(container.querySelectorAll('[data-testid="meeting-seat"]'));
        const roles = seats.map((s) => s.getAttribute('data-role'));
        expect(roles).toContain('pm');
        expect(roles).toContain('reviewer');
        expect(roles).toContain('researcher');
        const workerPersonas = seats
          .filter((s) => s.getAttribute('data-role') === 'worker')
          .map((s) => s.getAttribute('data-persona'));
        expect(workerPersonas).toEqual(['nova', 'mika']);

        // First transcript line (nova) is playing at mount (tick 0) — nova's seat is
        // highlighted and carries the bubble.
        const novaSeat = seats.find((s) => s.getAttribute('data-persona') === 'nova')!;
        expect(novaSeat.getAttribute('data-speaking')).toBe('true');
        expect(container.querySelector('[data-testid="meeting-bubble"]')?.textContent).toBe('built the client');

        // Advance exactly one line's worth of ticks (13 ticks * 200ms/tick) — the reviewer's
        // line is now playing instead.
        act(() => {
          vi.advanceTimersByTime(13 * 200);
        });
        const reviewerSeat = container.querySelector('[data-testid="meeting-seat"][data-role="reviewer"]')!;
        expect(reviewerSeat.getAttribute('data-speaking')).toBe('true');
        expect(novaSeat.getAttribute('data-speaking')).toBe('false');
        expect(container.querySelector('[data-testid="meeting-bubble"]')?.textContent).toBe('2 tasks passed');

        // One more line -> the researcher (their existing bookshelf-lane spot, not a table seat).
        act(() => {
          vi.advanceTimersByTime(13 * 200);
        });
        const researcherSeat = container.querySelector('[data-testid="meeting-seat"][data-role="researcher"]')!;
        expect(researcherSeat.getAttribute('data-speaking')).toBe('true');
      } finally {
        vi.useRealTimers();
      }
    });

    it('swaps back to the desk grid once the sprint leaves inReview (Active, no transcript)', () => {
      const activeProject: any = {
        ...reviewProject,
        sprints: [{ ...reviewProject.sprints[0], status: 'active', transcript: undefined }],
        activeSprint: { ...reviewProject.activeSprint, inReview: false },
      };
      act(() => {
        root.render(React.createElement(OfficeMap, { project: activeProject, onTaskClick: () => {} }));
      });
      expect(container.querySelectorAll('[data-testid="meeting-seat"][data-role="pm"]').length).toBe(0);
      expect(container.querySelectorAll('[data-testid="meeting-seat"][data-role="worker"]').length).toBe(0);
      expect(container.querySelectorAll('[data-testid="office-desk"]').length).toBeGreaterThan(0);
    });

    it('renders the sprint badge as "sprint i/N — goal" (1-indexed)', () => {
      act(() => {
        root.render(React.createElement(OfficeMap, { project: reviewProject, onTaskClick: () => {} }));
      });
      const badge = container.querySelector('[data-testid="sprint-badge"]');
      expect(badge?.textContent).toContain('sprint 1/1');
      expect(badge?.textContent).toContain('Foundation');
    });
  });

  // -------------------------------------------------------------------------
  // Ambient idle life (feature: ambient-idle-life)
  // -------------------------------------------------------------------------

  describe('ambient idle life', () => {
    it('never renders an idle sprite for a persona occupying an active desk', () => {
      const mixed: any = {
        id: 'm',
        name: 'M',
        phase: { kind: 'running' },
        config: { maxWorkers: 2 },
        tasks: [{ id: 'm/a', title: 'A', state: 'onprogress', priority: 5, persona: 'nova' }],
      };
      act(() => {
        root.render(React.createElement(OfficeMap, { project: mixed, onTaskClick: () => {} }));
      });
      const idlePersonas = Array.from(container.querySelectorAll('[data-testid="idle-sprite"]')).map((el) =>
        el.getAttribute('data-persona'),
      );
      expect(idlePersonas.length).toBeGreaterThan(0);
      expect(idlePersonas).not.toContain('nova');
    });

    it('never renders an idle sprite for a persona seated at the meeting table during a review', () => {
      // nova/mika's sprint tasks are Done — presenceFor alone would read them as idle — but
      // they are meeting attendees, so they must not double up as ambient idle sprites too.
      act(() => {
        root.render(React.createElement(OfficeMap, { project: reviewProject, onTaskClick: () => {} }));
      });
      const idlePersonas = Array.from(container.querySelectorAll('[data-testid="idle-sprite"]')).map((el) =>
        el.getAttribute('data-persona'),
      );
      expect(idlePersonas).not.toContain('nova');
      expect(idlePersonas).not.toContain('mika');
    });

    // -----------------------------------------------------------------------
    // Population cap (live-test scope amendment): the office only employs `maxWorkers` bodies
    // total (desks + idle wanderers combined) — the roster is 10, but a small project must not
    // spawn the whole unused roster as ambient sprites.
    // -----------------------------------------------------------------------

    it('max_workers=2, one working -> exactly one idle sprite renders', () => {
      const capped: any = {
        id: 'c',
        name: 'C',
        phase: { kind: 'running' },
        config: { maxWorkers: 2 },
        tasks: [{ id: 'c/a', title: 'A', state: 'onprogress', priority: 5, persona: 'nova' }],
      };
      act(() => {
        root.render(React.createElement(OfficeMap, { project: capped, onTaskClick: () => {} }));
      });
      const idleSprites = container.querySelectorAll('[data-testid="idle-sprite"]');
      expect(idleSprites.length).toBe(1);
      expect(idleSprites[0].getAttribute('data-persona')).not.toBe('nova');
    });

    it('max_workers=2, both working -> zero idle sprites (no roster spillover)', () => {
      const bothWorking: any = {
        id: 'w2',
        name: 'W2',
        phase: { kind: 'running' },
        config: { maxWorkers: 2 },
        tasks: [
          { id: 'w2/a', title: 'A', state: 'onprogress', priority: 5, persona: 'nova' },
          { id: 'w2/b', title: 'B', state: 'onprogress', priority: 4, persona: 'mika' },
        ],
      };
      act(() => {
        root.render(React.createElement(OfficeMap, { project: bothWorking, onTaskClick: () => {} }));
      });
      expect(container.querySelectorAll('[data-testid="idle-sprite"]').length).toBe(0);
    });
  });
});
