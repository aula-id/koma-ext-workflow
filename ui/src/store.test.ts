import { describe, it, expect, beforeEach, vi } from 'vitest';
import { useStore } from './store';
import { Bridge } from './bridge';

/**
 * Regression for the task-state wire-shape bug: office-core digest.rs
 * `state_label` (crates/office-core/src/digest.rs:150-159) serializes
 * `Task.state` as a plain lowercase string ("done", "onprogress", "parked",
 * ...), per docs/PANEL_PROTOCOL.md 2.2 -- the same shape Card.tsx/Board.tsx
 * already consume (`task.state === 'onprogress'`). The store used to probe
 * `t.state?.Done` / `t.state?.OnProgress` / `t.state?.Parked` as if `state`
 * were a serde-tagged-enum object, which it never is on the wire, so every
 * rollup count was permanently 0.
 */
describe('updateSnapshot task-state rollups', () => {
  beforeEach(() => {
    useStore.setState({ snapshot: null, projects: [] });
  });

  it('counts done/running/parked tasks from the real lowercase-string wire shape', () => {
    useStore.getState().updateSnapshot({
      kind: 'snapshot',
      seq: 1,
      projects: [
        {
          id: 'p1',
          name: 'Alpha',
          phase: { kind: 'running' },
          tasks: [
            { id: 't1', title: 'a', state: 'done' },
            { id: 't2', title: 'b', state: 'onprogress' },
            { id: 't3', title: 'c', state: 'parked' },
            { id: 't4', title: 'd', state: 'backlog' },
          ],
        },
      ],
    });

    const project = useStore.getState().getProject('p1')!;
    expect(project.taskCount).toBe(4);
    expect(project.doneCount).toBe(1);
    expect(project.runningCount).toBe(1);
    expect(project.parkedCount).toBe(1);
  });
});

/**
 * Regression for a zustand double-`setState` self-clobber: every view
 * (Dashboard.tsx/Board.tsx/App.tsx) wires `bridge.onSnapshot` to push data into the
 * store. That listener used to read:
 *
 *   useStore.setState((state) => { state.updateSnapshot(snap); return state; })
 *
 * `state.updateSnapshot(snap)` already calls the store's own `set(...)` internally
 * (a complete, correct update). But the OUTER `setState` call then takes the
 * function's return value -- `state`, the STALE pre-call snapshot captured before
 * `updateSnapshot` ran -- and merges it back on top, silently reverting the projects
 * list to what it was before the push. Net effect: no snapshot ever visibly reached
 * any view. Found while wiring the mock harness (?mock=1) and fixed by calling the
 * store action directly (`useStore.getState().updateSnapshot(snap)`), which this test
 * pins down by exercising the real `bridge.onSnapshot` -> store wiring end to end.
 */
describe('bridge push -> store wiring does not self-revert', () => {
  beforeEach(() => {
    useStore.setState({ snapshot: null, projects: [] });
  });

  it('a snapshot pushed through bridge.onSnapshot is visible in the store afterward', () => {
    const mockKomaPanel: any = {
      send: vi.fn(),
      onPush: vi.fn((handler: (payload: any) => void) => {
        mockKomaPanel._pushHandler = handler;
      }),
    };
    (window as any).KomaPanel = mockKomaPanel;
    const bridge = new Bridge();

    // The exact call-site pattern the views use post-fix.
    bridge.onSnapshot((snap) => {
      useStore.getState().updateSnapshot(snap);
    });

    mockKomaPanel._pushHandler({
      kind: 'snapshot',
      seq: 1,
      projects: [{ id: 'p1', name: 'Alpha', phase: { kind: 'running' }, tasks: [] }],
    });

    expect(useStore.getState().projects).toHaveLength(1);
    expect(useStore.getState().getProject('p1')?.name).toBe('Alpha');
  });

  it('demonstrates the old wrapper pattern would have reverted the push (documents the bug)', () => {
    const before = useStore.getState();

    // The buggy pattern this regression replaces: the outer setState's return value
    // (`state`, captured pre-mutation) gets merged back on top of the correctly-updated
    // state that `updateSnapshot`'s own inner `set()` call just applied.
    useStore.setState((state) => {
      state.updateSnapshot({
        kind: 'snapshot',
        seq: 1,
        projects: [{ id: 'p1', name: 'Alpha', phase: { kind: 'running' }, tasks: [] }],
      });
      return state;
    });

    // The push never sticks: projects reverts to whatever it was before this call.
    expect(useStore.getState().projects).toEqual(before.projects);
  });
});

/**
 * `officeActivity` (6.2d): an optional field on the full-mode snapshot, present only while
 * a live office activity is in flight and omitted entirely (not null) when idle.
 * `updateSnapshot` must round-trip it when present and default to `null` when absent.
 */
describe('updateSnapshot officeActivity', () => {
  beforeEach(() => {
    useStore.setState({ snapshot: null, projects: [] });
  });

  it('round-trips officeActivity when present in the raw snapshot', () => {
    useStore.getState().updateSnapshot({
      kind: 'snapshot',
      seq: 1,
      projects: [
        {
          id: 'p1',
          name: 'Alpha',
          phase: { kind: 'drafting' },
          tasks: [],
          officeActivity: { label: 'drafting the TRD', sinceMs: 123 },
        },
      ],
    });

    expect(useStore.getState().getProject('p1')?.officeActivity).toEqual({
      label: 'drafting the TRD',
      sinceMs: 123,
    });
  });

  it('defaults officeActivity to null when absent from the raw snapshot', () => {
    useStore.getState().updateSnapshot({
      kind: 'snapshot',
      seq: 1,
      projects: [
        { id: 'p1', name: 'Alpha', phase: { kind: 'drafting' }, tasks: [] },
      ],
    });

    expect(useStore.getState().getProject('p1')?.officeActivity).toBeNull();
  });
});
