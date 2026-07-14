import { describe, it, expect, beforeEach } from 'vitest';
import { useStore } from './store';

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
