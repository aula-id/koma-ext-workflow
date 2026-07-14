import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import { useStore } from '../store';
import Dashboard from './Dashboard';

/**
 * Regression for the phase-shape bug: the daemon serializes Project.phase as an
 * object `{ kind, reason?, atMs? }` (docs/PANEL_PROTOCOL.md 2.1 + office-core
 * digest.rs `phase_value`), not a capitalized string. Dashboard used to render
 * `{project.phase}` directly, which throws React's "Objects are not valid as a
 * React child" the instant a real snapshot arrives — crashing the landing view.
 */
describe('Dashboard phase rendering', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    // Let React flush effects/errors synchronously inside act(...).
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    // Reset the zustand singleton between tests.
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

  it('renders a real object-shaped phase without crashing', () => {
    // Seed the store exactly as bridge.ts forwards a daemon snapshot: phase is
    // the raw object, never a string.
    act(() => {
      useStore.getState().updateSnapshot({
        kind: 'snapshot',
        seq: 1,
        projects: [
          { id: 'p1', name: 'Alpha', phase: { kind: 'running' }, tasks: [] },
        ],
      });
    });

    // Before the fix this render throws "Objects are not valid as a React child
    // (found: object with keys {kind})".
    expect(() =>
      act(() => {
        root.render(React.createElement(Dashboard));
      }),
    ).not.toThrow();

    expect(container.textContent).toContain('Alpha');
    expect(container.textContent).toContain('running');
  });

  it('normalizes phase to the object shape in the store', () => {
    act(() => {
      useStore.getState().updateSnapshot({
        kind: 'snapshot',
        seq: 2,
        projects: [
          { id: 'p2', name: 'Beta', phase: { kind: 'halted', reason: 'blocked' }, tasks: [] },
        ],
      });
    });

    const project = useStore.getState().getProject('p2')!;
    expect(project.phase).toEqual({ kind: 'halted', reason: 'blocked' });
    expect(project.phase.kind).toBe('halted');
  });
});
