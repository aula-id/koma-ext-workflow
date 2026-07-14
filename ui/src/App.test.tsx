import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import React, { act } from 'react';
import { createRoot, Root } from 'react-dom/client';
import { useStore } from './store';
import App from './App';

/**
 * Regression for design-critique round 1: a `?view=board` (and drilldown/task/office,
 * which all resolve through the same `board` currentView) deep link flips
 * `currentView` to 'board' synchronously, but `selectedProject` only resolves once
 * the first snapshot arrives — a real async gap. Before the fix, the render ternary
 * had no branch for "board view, no project yet" and fell through to the `else`,
 * rendering the Dashboard's "Workflow" heading for that entire window. Any
 * screenshot/test landing inside that window saw the dashboard on every route.
 */
describe('App deep-link routing', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
    useStore.setState({ snapshot: null, projects: [] });
    window.history.pushState({}, '', '/?view=board');
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => {
      root.unmount();
    });
    container.remove();
    window.history.pushState({}, '', '/');
  });

  it('never falls back to rendering the Dashboard while the deep-linked project resolves', () => {
    act(() => {
      root.render(React.createElement(App));
    });

    // Before the project resolves: neither the dashboard heading nor a project's
    // Board should be present — just a neutral loading state.
    expect(container.textContent).not.toContain('Workflow');
    expect(container.textContent).not.toContain('3 projects active');

    // Once a snapshot with a running project lands, the deep link should resolve
    // straight into that project's Board (never having shown Dashboard first).
    act(() => {
      useStore.getState().updateSnapshot({
        kind: 'snapshot',
        seq: 1,
        projects: [
          {
            id: 'p1',
            name: 'Notifications Revamp',
            phase: { kind: 'running' },
            tasks: [],
          },
        ],
      });
    });

    expect(container.textContent).not.toContain('Workflow');
    expect(container.textContent).toContain('Notifications Revamp');
  });
});
