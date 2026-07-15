import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import OfficeMap from './OfficeMap';

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
});
